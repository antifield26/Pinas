use axum::{
    extract::{Extension, Query},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use argon2::password_hash::{rand_core::OsRng, PasswordHasher, SaltString};
use serde_json;
use sqlx::Row;
use argon2::Argon2;

use pinas_core::UserSession;
use crate::handlers::utils::log_audit;

// 获取用户配额（管理员可查看他人，普通用户只能查看自己的）—— 只读，不记录审计日志
pub async fn get_user_quota(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let target_user = params.get("username").unwrap_or(&session.username);
    if target_user != &session.username && session.role != "admin" {
        return (StatusCode::FORBIDDEN, "需要管理员权限").into_response();
    }
    let row = sqlx::query("SELECT quota_mb, used_mb FROM users WHERE username = ?")
        .bind(target_user)
        .fetch_optional(&pool)
        .await;
    match row {
        Ok(Some(r)) => {
            let quota: i64 = r.get("quota_mb");
            let used: i64 = r.get("used_mb");
            Json(serde_json::json!({ "quota_mb": quota, "used_mb": used })).into_response()
        }
        _ => (StatusCode::NOT_FOUND, "用户不存在").into_response(),
    }
}

// 设置用户配额（仅管理员）
pub async fn set_user_quota(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    if session.role != "admin" {
        return (StatusCode::FORBIDDEN, "需要管理员权限").into_response();
    }
    let username = payload.get("username").and_then(|v| v.as_str()).unwrap_or("");
    let quota_mb = payload.get("quota_mb").and_then(|v| v.as_i64());
    if username.is_empty() || quota_mb.is_none() {
        return (StatusCode::BAD_REQUEST, "缺少 username 或 quota_mb").into_response();
    }
    let old_quota: i64 = sqlx::query_scalar("SELECT quota_mb FROM users WHERE username = ?")
        .bind(username)
        .fetch_one(&pool)
        .await
        .unwrap_or(0);
    let result = sqlx::query("UPDATE users SET quota_mb = ? WHERE username = ?")
        .bind(quota_mb.unwrap())
        .bind(username)
        .execute(&pool)
        .await;
    match result {
        Ok(_) => {
            // 审计日志：修改配额
            let details = format!("{} MB -> {} MB", old_quota, quota_mb.unwrap());
            let _ = log_audit(&pool, &session.username, "set_quota", Some(username), Some(&details)).await;
            (StatusCode::OK, "配额更新成功").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("更新失败: {}", e)).into_response(),
    }
}

// 列出所有用户（仅管理员）—— 只读，不记录审计日志
pub async fn list_users(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> impl IntoResponse {
    if session.role != "admin" {
        return (StatusCode::FORBIDDEN, "需要管理员权限").into_response();
    }
    let rows = sqlx::query(
        "SELECT username, role, used_mb, quota_mb FROM users ORDER BY username"
    )
    .fetch_all(&pool)
    .await;
    match rows {
        Ok(users) => {
            let mut list = Vec::new();
            for row in users {
                let username: String = row.get("username");
                let role: String = row.get("role");
                let used_mb: i64 = row.get("used_mb");
                let quota_mb: i64 = row.get("quota_mb");
                list.push(serde_json::json!({
                    "username": username,
                    "role": role,
                    "used_mb": used_mb,
                    "quota_mb": quota_mb,
                }));
            }
            Json(list).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("查询失败: {}", e)).into_response(),
    }
}

// 重置用户密码（仅管理员）
pub async fn reset_user_password(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    if session.role != "admin" {
        return (StatusCode::FORBIDDEN, "需要管理员权限").into_response();
    }
    let username = payload.get("username").and_then(|v| v.as_str()).unwrap_or("");
    let new_password = payload.get("new_password").and_then(|v| v.as_str()).unwrap_or("");
    if username.is_empty() || new_password.is_empty() {
        return (StatusCode::BAD_REQUEST, "缺少 username 或 new_password").into_response();
    }
    if new_password.len() < 6 {
        return (StatusCode::BAD_REQUEST, "密码长度至少为 6 位").into_response();
    }
    let salt = SaltString::generate(&mut OsRng);
    let hashed = match Argon2::default().hash_password(new_password.as_bytes(), &salt) {
        Ok(h) => h.to_string(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("密码哈希失败: {}", e)).into_response(),
    };
    let result = sqlx::query("UPDATE users SET password = ? WHERE username = ?")
        .bind(&hashed)
        .bind(username)
        .execute(&pool)
        .await;
    // 清除该用户的所有会话（强制重新登录）
    let _ = sqlx::query("DELETE FROM sessions WHERE username = ?")
        .bind(username)
        .execute(&pool)
        .await;

    match result {
        Ok(_) => {
            // 审计日志：重置密码
            let _ = log_audit(&pool, &session.username, "reset_password", Some(username), None).await;
            (StatusCode::OK, "密码重置成功").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("更新失败: {}", e)).into_response(),
    }
}

// 获取审计日志（仅管理员）
pub async fn get_audit_logs(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    if session.role != "admin" {
        return (StatusCode::FORBIDDEN, "需要管理员权限").into_response();
    }
    let limit = params.get("limit").and_then(|l| l.parse::<i64>().ok()).unwrap_or(100);
    let offset = params.get("offset").and_then(|o| o.parse::<i64>().ok()).unwrap_or(0);
    let rows = sqlx::query(
        "SELECT id, username, action, target, details, ip_address, user_agent, created_at FROM audit_logs ORDER BY created_at DESC LIMIT ? OFFSET ?"
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(&pool)
    .await;
    match rows {
        Ok(logs) => {
            let list: Vec<serde_json::Value> = logs.iter().map(|row| {
                serde_json::json!({
                    "id": row.get::<i64, _>("id"),
                    "username": row.get::<String, _>("username"),
                    "action": row.get::<String, _>("action"),
                    "target": row.get::<String, _>("target"),
                    "details": row.get::<String, _>("details"),
                    "ip_address": row.get::<String, _>("ip_address"),
                    "user_agent": row.get::<String, _>("user_agent"),
                    "created_at": row.get::<String, _>("created_at"),
                })
            }).collect();
            Json(list).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("查询失败: {}", e)).into_response(),
    }
}