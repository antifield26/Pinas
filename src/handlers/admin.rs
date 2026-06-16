use axum::{
    extract::{Extension, Query},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Row};

use pinas_core::UserSession;
use crate::handlers::utils::{hash_password, log_audit};

// DTOs
#[derive(Deserialize)]
pub struct SetQuotaRequest {
    pub username: String,
    pub quota_mb: i64,
}

#[derive(Deserialize)]
pub struct ResetPasswordRequest {
    pub username: String,
    pub new_password: String,
}

#[derive(Serialize)]
pub struct QuotaInfo {
    pub quota_mb: i64,
    pub used_mb: i64,
}

#[derive(Serialize)]
pub struct UserInfo {
    pub username: String,
    pub role: String,
    pub used_mb: i64,
    pub quota_mb: i64,
}

// 获取用户配额（管理员可查看他人，普通用户只能查看自己的）—— 只读，不记录审计日志
pub async fn get_user_quota(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let target_user = params.get("username").unwrap_or(&session.username);
    if target_user != &session.username && session.role != crate::constants::ROLE_ADMIN {
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
            Json(QuotaInfo { quota_mb: quota, used_mb: used }).into_response()
        }
        _ => (StatusCode::NOT_FOUND, "用户不存在").into_response(),
    }
}

// 设置用户配额（仅管理员）
pub async fn set_user_quota(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<SetQuotaRequest>,
) -> impl IntoResponse {
    if session.role != crate::constants::ROLE_ADMIN {
        return (StatusCode::FORBIDDEN, "需要管理员权限").into_response();
    }
    if payload.username.is_empty() {
        return (StatusCode::BAD_REQUEST, "缺少 username").into_response();
    }
    let old_quota: i64 = sqlx::query_scalar("SELECT quota_mb FROM users WHERE username = ?")
        .bind(&payload.username)
        .fetch_one(&pool)
        .await
        .unwrap_or(0);
    let result = sqlx::query("UPDATE users SET quota_mb = ? WHERE username = ?")
        .bind(payload.quota_mb)
        .bind(&payload.username)
        .execute(&pool)
        .await;
    match result {
        Ok(_) => {
            let details = format!("{} MB -> {} MB", old_quota, payload.quota_mb);
            let _ = log_audit(&pool, &session.username, "set_quota", Some(&payload.username), Some(&details), None, None).await;
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
    if session.role != crate::constants::ROLE_ADMIN {
        return (StatusCode::FORBIDDEN, "需要管理员权限").into_response();
    }
    let rows = sqlx::query_as::<_, (String, String, i64, i64)>(
        "SELECT username, role, used_mb, quota_mb FROM users ORDER BY username"
    )
    .fetch_all(&pool)
    .await;
    match rows {
        Ok(users) => {
            let list: Vec<UserInfo> = users.into_iter().map(|(username, role, used_mb, quota_mb)| {
                UserInfo { username, role, used_mb, quota_mb }
            }).collect();
            Json(list).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("查询失败: {}", e)).into_response(),
    }
}

// 重置用户密码（仅管理员）
pub async fn reset_user_password(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<ResetPasswordRequest>,
) -> impl IntoResponse {
    if session.role != crate::constants::ROLE_ADMIN {
        return (StatusCode::FORBIDDEN, "需要管理员权限").into_response();
    }
    if payload.username.is_empty() || payload.new_password.is_empty() {
        return (StatusCode::BAD_REQUEST, "缺少 username 或 new_password").into_response();
    }
    if payload.new_password.len() < 6 {
        return (StatusCode::BAD_REQUEST, "密码长度至少为 6 位").into_response();
    }
    let pwd = payload.new_password.clone();
    let hashed = match tokio::task::spawn_blocking(move || hash_password(&pwd)).await {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "服务内部错误").into_response(),
    };
    let result = sqlx::query("UPDATE users SET password = ? WHERE username = ?")
        .bind(&hashed)
        .bind(&payload.username)
        .execute(&pool)
        .await;
    // 清除该用户的所有会话（强制重新登录）
    let _ = sqlx::query("DELETE FROM sessions WHERE username = ?")
        .bind(&payload.username)
        .execute(&pool)
        .await;

    match result {
        Ok(_) => {
            let _ = log_audit(&pool, &session.username, "reset_password", Some(&payload.username), None, None, None).await;
            (StatusCode::OK, "密码重置成功").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("更新失败: {}", e)).into_response(),
    }
}

// 审计日志项
#[derive(Serialize, FromRow)]
pub struct AuditLogItem {
    pub id: i64,
    pub username: String,
    pub action: String,
    pub target: Option<String>,
    pub details: Option<String>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: Option<String>,
}

// 获取审计日志（仅管理员）
pub async fn get_audit_logs(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    if session.role != crate::constants::ROLE_ADMIN {
        return (StatusCode::FORBIDDEN, "需要管理员权限").into_response();
    }
    let limit = params.get("limit").and_then(|l| l.parse::<i64>().ok()).unwrap_or(100);
    let offset = params.get("offset").and_then(|o| o.parse::<i64>().ok()).unwrap_or(0);
    match sqlx::query_as::<_, AuditLogItem>(
        "SELECT id, username, action, target, details, ip_address, user_agent, created_at FROM audit_logs ORDER BY created_at DESC LIMIT ? OFFSET ?"
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(&pool)
    .await
    {
        Ok(logs) => Json(logs).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("查询失败: {}", e)).into_response(),
    }
}