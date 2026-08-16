use axum::{
    extract::{Extension, Query},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Row};
use std::sync::LazyLock;

use crate::config::Config;
use crate::core::UserSession;
use crate::error::{AppError, AppResult};
use crate::handlers::utils::{hash_password, log_audit};
use sqlx::Connection;

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
#[tracing::instrument(skip_all)]
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
            Ok(Json(QuotaInfo {
                quota_mb: quota,
                used_mb: used,
            }))
        }
        None => Err(AppError::not_found("用户不存在")),
    }
}

// 设置用户配额（仅管理员）
#[tracing::instrument(skip_all)]
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
    let _ = log_audit(
        &pool,
        &session.username,
        "set_quota",
        Some(&payload.username),
        Some(&details),
        None,
        None,
    )
    .await;
    Ok((StatusCode::OK, "配额更新成功"))
}

// 列出所有用户（仅管理员）—— 只读，不记录审计日志
#[tracing::instrument(skip_all)]
pub async fn list_users(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> AppResult<Json<Vec<UserInfo>>> {
    if session.role != crate::constants::ROLE_ADMIN {
        return Err(AppError::forbidden("需要管理员权限"));
    }
    let users = sqlx::query_as::<_, (String, String, i64, i64)>(
        "SELECT username, role, used_mb, quota_mb FROM users ORDER BY username",
    )
    .fetch_all(&pool)
    .await?;
    let list: Vec<UserInfo> = users
        .into_iter()
        .map(|(username, role, used_mb, quota_mb)| UserInfo {
            username,
            role,
            used_mb,
            quota_mb,
        })
        .collect();
    Ok(Json(list))
}

// ====== HTMX Fragment ======
use crate::templates::AppTemplate;
use askama::Template;

#[derive(Template)]
#[template(path = "components/admin_users.html")]
struct AdminUsersFragment {
    users: Vec<UserInfo>,
}

#[tracing::instrument(skip_all)]
pub async fn admin_users_fragment(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> impl axum::response::IntoResponse {
    // 非管理员 403：与 JSON admin 端点一致（此前 200 空数据掩盖未授权状态）
    if session.role != crate::constants::ROLE_ADMIN {
        return StatusCode::FORBIDDEN.into_response();
    }
    let users = sqlx::query_as::<_, (String, String, i64, i64)>(
        "SELECT username, role, used_mb, quota_mb FROM users ORDER BY username",
    )
    .fetch_all(&pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(username, role, used_mb, quota_mb)| UserInfo {
        username,
        role,
        used_mb,
        quota_mb,
    })
    .collect();
    AppTemplate(AdminUsersFragment { users }).into_response()
}

// 重置用户密码（仅管理员）
#[tracing::instrument(skip_all)]
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
        .map_err(AppError::internal)?;
    // 事务内执行：先清除会话（强制重新登录），再更新密码
    let mut tx = pool.begin().await?;

    sqlx::query("DELETE FROM sessions WHERE username = ?")
        .bind(&payload.username)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            tracing::error!(
                "reset_password: 清除用户 {} 会话失败: {}",
                payload.username,
                e
            );
            AppError::internal("清除会话失败")
        })?;

    sqlx::query("UPDATE users SET password = ? WHERE username = ?")
        .bind(&hashed)
        .bind(&payload.username)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    // 密码已重置：失效 WebDAV 认证缓存（旧凭证不得再命中 60s 窗口）
    crate::handlers::dav::invalidate_dav_auth_cache(&payload.username);

    let _ = log_audit(
        &pool,
        &session.username,
        "reset_password",
        Some(&payload.username),
        None,
        None,
        None,
    )
    .await;
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
    let limit = params
        .get("limit")
        .and_then(|l| l.parse::<i64>().ok())
        .unwrap_or(100);
    let offset = params
        .get("offset")
        .and_then(|o| o.parse::<i64>().ok())
        .unwrap_or(0);
    let logs = sqlx::query_as::<_, AuditLogItem>(
        "SELECT id, username, action, target, details, ip_address, user_agent, created_at FROM audit_logs ORDER BY created_at DESC LIMIT ? OFFSET ?"
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(&pool)
    .await?;
    Ok(Json(logs))
}

// ====== 数据库备份 ======

const BACKUPS_DIR: &str = "backups";

/// 备份并发互斥（P1-3）：VACUUM INTO 在大库上耗时秒级~十秒级且持主库写锁，
/// 并发备份会让全站写操作 busy_timeout 后失败——单许可信号量串行化。
/// 采用等待（acquire）而非直接 409：备份低频，等待即可完成，用户期望拿到结果
static BACKUP_LOCK: LazyLock<tokio::sync::Semaphore> =
    LazyLock::new(|| tokio::sync::Semaphore::new(1));

/// POST /api/admin/backup — 创建数据库备份（仅管理员）
#[tracing::instrument(skip_all)]
pub async fn create_backup(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(config): Extension<Config>,
    Extension(session): Extension<UserSession>,
) -> AppResult<Json<serde_json::Value>> {
    if session.role != crate::constants::ROLE_ADMIN {
        return Err(AppError::forbidden("需要管理员权限"));
    }

    // P1-3：串行化备份——大库 VACUUM 秒级~十秒级，并发备份会互相 busy_timeout 冻结全站。
    // 等待（acquire）而非 409：备份低频，等待即可完成；guard 在函数结束（含 VACUUM）后自动释放
    let _guard = BACKUP_LOCK
        .acquire()
        .await
        .map_err(|e| AppError::internal_log("获取备份互斥锁", e))?;

    let _ = tokio::fs::create_dir_all(BACKUPS_DIR).await;
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let filename = format!("cloud_disk_backup_{}.db", timestamp);
    let backup_path = format!("{}/{}", BACKUPS_DIR, filename);

    // P1-3：VACUUM INTO 是阻塞 IO（大库秒级~十秒级），不能占住 tokio async worker。
    // SQLite 连接不可跨线程移动，故在 spawn_blocking 闭包内新建独立连接执行；
    // sqlx 连接 API 是 async，经 Handle::block_on 在当前阻塞线程驱动（不会被迁移到其他 worker）
    let db_path = config
        .database_url
        .strip_prefix("sqlite:")
        .unwrap_or("cloud_disk.db")
        .to_string();
    let backup_path_cloned = backup_path.clone();
    let res: Result<(), sqlx::Error> = tokio::task::spawn_blocking(move || {
        let opts = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(false);
        let mut conn = tokio::runtime::Handle::current()
            .block_on(sqlx::sqlite::SqliteConnection::connect_with(&opts))?;
        let sql = format!("VACUUM INTO '{}'", backup_path_cloned);
        let r = tokio::runtime::Handle::current()
            .block_on(sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).execute(&mut conn))
            .map(|_| ());
        let _ = tokio::runtime::Handle::current().block_on(conn.close());
        r
    })
    .await
    .map_err(|e| AppError::internal_log("备份任务异常终止", e))?;
    res?;
    let _ = log_audit(
        &pool,
        &session.username,
        "backup_create",
        Some(&filename),
        None,
        None,
        None,
    )
    .await;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "filename": filename,
        "size": std::fs::metadata(&backup_path).map(|m| m.len()).unwrap_or(0)
    })))
}

/// GET /api/admin/backup/list — 列出已有备份（仅管理员）
pub async fn list_backups(
    Extension(session): Extension<UserSession>,
) -> AppResult<Json<Vec<serde_json::Value>>> {
    if session.role != crate::constants::ROLE_ADMIN {
        return Err(AppError::forbidden("需要管理员权限"));
    }

    let mut backups = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(BACKUPS_DIR).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if let Ok(meta) = entry.metadata().await {
                backups.push(serde_json::json!({
                    "name": entry.file_name().to_string_lossy(),
                    "size": meta.len(),
                    "modified": meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0),
                }));
            }
        }
    }
    backups.sort_by(|a, b| b["modified"].as_u64().cmp(&a["modified"].as_u64()));
    Ok(Json(backups))
}

/// GET /api/admin/backup/download?file= — 下载备份文件（仅管理员）
pub async fn download_backup(
    Extension(session): Extension<UserSession>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<impl IntoResponse, AppError> {
    if session.role != crate::constants::ROLE_ADMIN {
        return Err(AppError::forbidden("需要管理员权限"));
    }

    let filename = params
        .get("file")
        .ok_or_else(|| AppError::bad_request("缺少 file 参数"))?;
    // P2-6：严格白名单——备份文件名由服务端生成（cloud_disk_backup_{YYYYmmdd_HHMMSS}.db，
    // 如 cloud_disk_backup_20260801_120000.db），仅允许 [A-Za-z0-9._-]+ 且以 .db 结尾。
    // 纯白名单校验天然排除路径穿越（`/`、`..`）、符号链接与任意文件读取
    if !filename.ends_with(".db")
        || !filename
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(AppError::bad_request("非法文件名"));
    }
    let path = format!("{}/{}", BACKUPS_DIR, filename);
    if !std::path::Path::new(&path).exists() {
        return Err(AppError::not_found("备份文件不存在"));
    }

    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| AppError::internal("无法读取备份文件"))?;
    let stream = tokio_util::io::ReaderStream::new(file);
    let body = axum::body::Body::from_stream(stream);

    use axum::http::header;
    let mut resp = axum::response::Response::new(body);
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/octet-stream".parse().unwrap(),
    );
    resp.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{}\"", filename)
            .parse()
            .unwrap(),
    );

    Ok(resp)
}
