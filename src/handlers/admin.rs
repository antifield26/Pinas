use axum::{
    extract::{Extension, Query},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Row};

use pinas_core::UserSession;
use crate::error::{AppError, AppResult};
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
) -> AppResult<Json<QuotaInfo>> {
    let target_user = params.get("username").unwrap_or(&session.username);
    if target_user != &session.username && session.role != crate::constants::ROLE_ADMIN {
        return Err(AppError::forbidden("需要管理员权限"));
    }
    let row = sqlx::query("SELECT quota_mb, used_mb FROM users WHERE username = ?")
        .bind(target_user)
        .fetch_optional(&pool)
        .await?;
    match row {
        Some(r) => {
            let quota: i64 = r.get("quota_mb");
            let used: i64 = r.get("used_mb");
            Ok(Json(QuotaInfo { quota_mb: quota, used_mb: used }))
        }
        None => Err(AppError::not_found("用户不存在")),
    }
}

// 设置用户配额（仅管理员）
pub async fn set_user_quota(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<SetQuotaRequest>,
) -> AppResult<(StatusCode, &'static str)> {
    if session.role != crate::constants::ROLE_ADMIN {
        return Err(AppError::forbidden("需要管理员权限"));
    }
    if payload.username.is_empty() {
        return Err(AppError::bad_request("缺少 username"));
    }
    let old_quota: i64 = sqlx::query_scalar("SELECT quota_mb FROM users WHERE username = ?")
        .bind(&payload.username)
        .fetch_one(&pool)
        .await
        .unwrap_or(0);
    sqlx::query("UPDATE users SET quota_mb = ? WHERE username = ?")
        .bind(payload.quota_mb)
        .bind(&payload.username)
        .execute(&pool)
        .await?;
    let details = format!("{} MB -> {} MB", old_quota, payload.quota_mb);
    let _ = log_audit(&pool, &session.username, "set_quota", Some(&payload.username), Some(&details), None, None).await;
    Ok((StatusCode::OK, "配额更新成功"))
}

// 列出所有用户（仅管理员）—— 只读，不记录审计日志
pub async fn list_users(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> AppResult<Json<Vec<UserInfo>>> {
    if session.role != crate::constants::ROLE_ADMIN {
        return Err(AppError::forbidden("需要管理员权限"));
    }
    let users = sqlx::query_as::<_, (String, String, i64, i64)>(
        "SELECT username, role, used_mb, quota_mb FROM users ORDER BY username"
    )
    .fetch_all(&pool)
    .await?;
    let list: Vec<UserInfo> = users.into_iter().map(|(username, role, used_mb, quota_mb)| {
        UserInfo { username, role, used_mb, quota_mb }
    }).collect();
    Ok(Json(list))
}

// 重置用户密码（仅管理员）
pub async fn reset_user_password(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<ResetPasswordRequest>,
) -> AppResult<(StatusCode, &'static str)> {
    if session.role != crate::constants::ROLE_ADMIN {
        return Err(AppError::forbidden("需要管理员权限"));
    }
    if payload.username.is_empty() || payload.new_password.is_empty() {
        return Err(AppError::bad_request("缺少 username 或 new_password"));
    }
    if payload.new_password.len() < 6 {
        return Err(AppError::bad_request("密码长度至少为 6 位"));
    }
    let pwd = payload.new_password.clone();
    let hashed = tokio::task::spawn_blocking(move || hash_password(&pwd))
        .await
        .map_err(|_| AppError::internal("服务内部错误"))?
        .map_err(|e| AppError::internal(e))?;
    // 事务内执行：先清除会话（强制重新登录），再更新密码
    let mut tx = pool.begin().await?;

    sqlx::query("DELETE FROM sessions WHERE username = ?")
        .bind(&payload.username)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            tracing::error!("reset_password: 清除用户 {} 会话失败: {}", payload.username, e);
            AppError::internal("清除会话失败")
        })?;

    sqlx::query("UPDATE users SET password = ? WHERE username = ?")
        .bind(&hashed)
        .bind(&payload.username)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    let _ = log_audit(&pool, &session.username, "reset_password", Some(&payload.username), None, None, None).await;
    Ok((StatusCode::OK, "密码重置成功"))
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
) -> AppResult<Json<Vec<AuditLogItem>>> {
    if session.role != crate::constants::ROLE_ADMIN {
        return Err(AppError::forbidden("需要管理员权限"));
    }
    let limit = params.get("limit").and_then(|l| l.parse::<i64>().ok()).unwrap_or(100);
    let offset = params.get("offset").and_then(|o| o.parse::<i64>().ok()).unwrap_or(0);
    let logs = sqlx::query_as::<_, AuditLogItem>(
        "SELECT id, username, action, target, details, ip_address, user_agent, created_at FROM audit_logs ORDER BY created_at DESC LIMIT ? OFFSET ?"
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(&pool)
    .await?;
    Ok(Json(logs))
}