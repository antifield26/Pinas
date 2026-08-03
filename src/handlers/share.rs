use axum::{
    body::Body,
    extract::{Extension, Path, Query},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Json},
};
use chrono::{Utc, NaiveDateTime};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Row};
use uuid::Uuid;
use tokio_util::io::ReaderStream;

use crate::error::{AppError, AppResult};
use crate::handlers::utils::{hash_password, safe_join_sandbox, verify_password, log_audit};
use pinas_core::UserSession;

// --- DTOs ---
#[derive(Deserialize)]
pub struct CreateShareRequest {
    pub file_path: String,
    pub is_dir: bool,
    pub expire_hours: Option<u32>,
    pub password: Option<String>,
}

#[derive(Deserialize)]
pub struct AccessShareRequest {
    pub password: Option<String>,
}

#[derive(Serialize, FromRow)]
pub struct ShareItem {
    pub code: String,
    pub file_path: String,
    pub is_dir: i64,
    pub username: String,
    pub expires_at: Option<String>,
    pub has_password: i64,
    pub download_count: i64,
}

// --- 12. 创建分享 ---
#[tracing::instrument(skip_all)]
pub async fn create_share(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<CreateShareRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let share_code = Uuid::new_v4().to_string();
    let is_dir_val = if payload.is_dir { 1 } else { 0 };

    let password_hash = match &payload.password {
        Some(pwd) if !pwd.is_empty() => {
            let pwd = pwd.clone();
            Some(tokio::task::spawn_blocking(move || hash_password(&pwd)).await
                .map_err(|_| AppError::internal("服务内部错误"))?
                .map_err(AppError::internal)?)
        }
        _ => None,
    };
    let has_pwd = if password_hash.is_some() { 1 } else { 0 };

    let expires_at = payload.expire_hours.map(|h| {
        chrono::Utc::now().checked_add_signed(chrono::Duration::hours(h as i64))
            .unwrap_or_else(|| chrono::Utc::now() + chrono::Duration::hours(24))
            .format("%Y-%m-%d %H:%M:%S").to_string()
    });

    sqlx::query(
        "INSERT INTO shares (code, file_path, is_dir, username, expires_at, password, has_password) VALUES (?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&share_code).bind(&payload.file_path).bind(is_dir_val).bind(&session.username)
    .bind(&expires_at).bind(&password_hash).bind(has_pwd).execute(&pool).await?;

    let _ = log_audit(&pool, &session.username, "create_share", Some(&payload.file_path), None, None, None).await;
    Ok(Json(serde_json::json!({ "code": share_code })))
}

// --- 13. 列出当前用户的全量分享 ---
#[tracing::instrument(skip_all)]
pub async fn list_shares(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> AppResult<Json<Vec<ShareItem>>> {
    let rows = sqlx::query_as::<_, ShareItem>(
        "SELECT code, file_path, is_dir, username, expires_at, has_password, download_count FROM shares WHERE username = ?"
    ).bind(&session.username).fetch_all(&pool).await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct DeleteShareRequest {
    pub code: String,
}

// --- 14. 删除分享外链 ---
#[tracing::instrument(skip_all)]
pub async fn delete_share(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<DeleteShareRequest>,
) -> AppResult<(StatusCode, &'static str)> {
    sqlx::query("DELETE FROM shares WHERE code = ? AND username = ?")
        .bind(&payload.code).bind(&session.username).execute(&pool).await?;
    let _ = log_audit(&pool, &session.username, "delete_share", Some(&payload.code), None, None, None).await;
    Ok((StatusCode::OK, "分享链条已切断"))
}

// --- 19. 外链匿名提取与子目录探索（简单响应）---
pub async fn access_share(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Path(code): Path<String>,
) -> AppResult<(StatusCode, &'static str)> {
    let exists = sqlx::query("SELECT 1 FROM shares WHERE code = ? AND (expires_at IS NULL OR expires_at > datetime('now'))")
        .bind(&code).fetch_optional(&pool).await?.is_some();
    if exists { Ok((StatusCode::OK, "分享链接有效")) }
    else { Err(AppError::gone("外链已失效或过期")) }
}

// --- 分享页面入口 ---
use askama::Template;
use crate::templates::AppTemplate;

#[derive(Template)]
#[template(path = "pages/share.html")]
struct SharePage {
    share_id: String,
    file_path: String,
    file_size: String,
    file_count: usize,
    is_dir: bool,
    password_required: bool,
    password: String,
}

pub async fn share_page(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Path(share_id): Path<String>,
    Query(params): Query<AccessShareRequest>,
) -> impl IntoResponse {
    let code = &share_id;
    // Look up share info
    let share = sqlx::query(
        "SELECT file_path, password, expires_at FROM shares WHERE code = ?"
    )
    .bind(code)
    .fetch_optional(&pool)
    .await
    .unwrap_or(None);

    let (file_path_str, stored_password, expires_at): (String, Option<String>, Option<String>) = match share {
        Some(row) => (
            row.get::<String, _>(0),
            row.get::<Option<String>, _>(1),
            row.get::<Option<String>, _>(2),
        ),
        None => return (StatusCode::NOT_FOUND, "分享链接不存在或已失效").into_response(),
    };

    // Check expiry
    if let Some(exp) = &expires_at {
        if let Ok(exp_dt) = chrono::NaiveDateTime::parse_from_str(exp, "%Y-%m-%d %H:%M:%S") {
            if exp_dt < Utc::now().naive_utc() {
                return (StatusCode::GONE, "分享链接已过期").into_response();
            }
        }
    }

    let stored_pwd = stored_password.clone().unwrap_or_default();
    let password_required = !stored_pwd.is_empty();
    let submitted_password = params.password.clone().unwrap_or_default();

    // If password protected and wrong/missing password, show password form
    // Use spawn_blocking for Argon2 verification to avoid blocking async runtime
    let pwd_ok = if password_required {
        let hash = stored_pwd.clone();
        let input = submitted_password.clone();
        tokio::task::spawn_blocking(move || pinas_core::verify_password(&hash, &input))
            .await
            .unwrap_or(false)
    } else { true };
    if password_required && !pwd_ok {
        return AppTemplate(SharePage {
            share_id: share_id.clone(),
            file_path: file_path_str,
            file_size: String::new(),
            file_count: 0,
            is_dir: false,
            password_required: true,
            password: String::new(),
        }).into_response();
    }

    // Check if it's a directory
    let base = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let full_path = crate::handlers::utils::safe_join_sandbox(base, &file_path_str);
    let is_dir = full_path.is_dir();
    let file_size;
    let file_count;

    if is_dir {
        file_count = std::fs::read_dir(&full_path).map(|d| d.count()).unwrap_or(0);
        file_size = format!("{} 个项目", file_count);
    } else {
        let meta = full_path.metadata();
        let len = meta.map(|m| m.len()).unwrap_or(0);
        file_size = if len < 1024 { format!("{} B", len) }
            else if len < 1024 * 1024 { format!("{:.1} KB", len as f64 / 1024.0) }
            else { format!("{:.1} MB", len as f64 / 1024.0 / 1024.0) };
        file_count = 0;
    }

    AppTemplate(SharePage {
        share_id: share_id.clone(),
        file_path: file_path_str,
        file_size,
        file_count,
        is_dir,
        password_required: false,
        password: submitted_password,
    }).into_response()
}

// --- 访问分享下的具体文件或子目录 ---
pub async fn share_subfile(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Path((share_id, file_path)): Path<(String, String)>,
    Query(params): Query<AccessShareRequest>,
) -> impl IntoResponse {
    use axum::http::header;

    // 1. 查询分享信息
    let share_row = sqlx::query(
        "SELECT username, file_path, is_dir, password, expires_at FROM shares WHERE code = ?"
    )
    .bind(&share_id)
    .fetch_optional(&pool)
    .await;

    let share = match share_row {
        Ok(Some(row)) => row,
        _ => return (StatusCode::NOT_FOUND, "分享不存在或已失效").into_response(),
    };

    let username: String = share.get("username");
    let share_root_path: String = share.get("file_path");
    let db_password: Option<String> = share.get("password");
    let expires_at: Option<String> = share.get("expires_at");

    // 2. 校验密码（如果有）— spawn_blocking 避免阻塞
    if let Some(pwd_hash) = db_password {
        let input_pwd = params.password.unwrap_or_default().to_string();
        let hash_clone = pwd_hash.clone();
        let is_valid = tokio::task::spawn_blocking(move || {
            verify_password(&hash_clone, &input_pwd)
        }).await.unwrap_or(false);
        if !is_valid {
            return (StatusCode::UNAUTHORIZED, "密码错误").into_response();
        }
    }

    // 3. 校验有效期
    if let Some(expire_str) = expires_at {
        if let Ok(expire) = NaiveDateTime::parse_from_str(&expire_str, "%Y-%m-%d %H:%M:%S") {
            if Utc::now().naive_utc() > expire {
                return (StatusCode::GONE, "分享链接已过期").into_response();
            }
        }
    }

    let user_root = std::path::PathBuf::from(crate::constants::UPLOADS_DIR).join(&username);
    let share_base = safe_join_sandbox(&user_root, &share_root_path);
    let target_path = safe_join_sandbox(&share_base, &file_path);

    // 路径前缀检查（组件级，阻止 ../ 穿越）
    if !target_path.starts_with(&share_base) {
        return (StatusCode::FORBIDDEN, "访问越界").into_response();
    }
    if !target_path.exists() {
        return (StatusCode::NOT_FOUND, "文件或目录不存在").into_response();
    }
    // 符号链接兜底：文件存在后，规范化路径再次检查
    if let (Ok(canon_target), Ok(canon_base)) = (target_path.canonicalize(), share_base.canonicalize()) {
        if !canon_target.starts_with(&canon_base) {
            tracing::error!("[Share] 符号链接越界: target={:?}, base={:?}", canon_target, canon_base);
            return (StatusCode::FORBIDDEN, "访问越界").into_response();
        }
    }

    // 4. 如果是目录，返回目录内容 JSON
    if target_path.is_dir() {
        let items = list_directory_files(&target_path, &user_root).await;
        return (StatusCode::OK, Json(items)).into_response();
    }

    // 增加下载计数 + 审计（异步，不阻塞响应）
    let share_code = share_id.clone();
    let pool_clone = pool.clone();
    let file = file_path.clone();
    tokio::spawn(async move {
        let _ = sqlx::query("UPDATE shares SET download_count = download_count + 1 WHERE code = ?")
            .bind(&share_code).execute(&pool_clone).await;
        let _ = crate::handlers::log_audit(&pool_clone, &username, "share_download", Some(&file), None, None, None).await;
    });

    // 5. 如果是文件，流式返回
    match tokio::fs::File::open(&target_path).await {
        Ok(file) => {
            let mime = mime_guess::from_path(&target_path).first_or_octet_stream();
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                mime.to_string().parse().unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
            );
            headers.insert(
                header::ACCEPT_RANGES,
                HeaderValue::from_static("bytes"),
            );
            let body = Body::from_stream(ReaderStream::new(file));
            (StatusCode::OK, headers, body).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "文件读取失败").into_response(),
    }
}

// --- 辅助函数：列出目录内容（供分享使用）---
async fn list_directory_files(dir_path: &std::path::Path, user_root: &std::path::Path) -> Vec<serde_json::Value> {
    let mut items = Vec::new();
    let mut entries = match tokio::fs::read_dir(dir_path).await {
        Ok(e) => e,
        Err(_) => return items,
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        let metadata = match entry.metadata().await {
            Ok(m) => m,
            Err(_) => continue,
        };
        let relative_path = match entry.path().strip_prefix(user_root) {
            Ok(p) => p.to_string_lossy().into_owned(),
            Err(_) => continue,
        };
        items.push(serde_json::json!({
            "name": name,
            "is_dir": metadata.is_dir(),
            "size": metadata.len(),
            "path": relative_path,
        }));
    }
    items
}