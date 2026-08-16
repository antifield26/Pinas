use axum::{
    body::Body,
    extract::{Extension, Path, Query},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Json},
};
use chrono::{NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Row};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::core::UserSession;
use crate::error::{AppError, AppResult};
use crate::handlers::utils::{hash_password, log_audit, safe_join_sandbox, verify_password};

// --- 匿名分享端点的防护（分享页公开无认证，必须独立限速） ---
use crate::handlers::{MaybePeer, extract_ip, rate_limit};
use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Instant;
use tokio::sync::Mutex;

/// 每分享密码失败尝试锁定：同一分享 5 次错误密码 → 锁定 15 分钟（防无限速爆破）
static SHARE_FAILED: LazyLock<Mutex<HashMap<String, (u32, Instant)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

const SHARE_MAX_FAILURES: u32 = 5;
const SHARE_LOCKOUT_SECS: u64 = 15 * 60;

async fn share_check_locked(code: &str) -> bool {
    let map = SHARE_FAILED.lock().await;
    match map.get(code) {
        Some((n, t)) => *n >= SHARE_MAX_FAILURES && t.elapsed().as_secs() < SHARE_LOCKOUT_SECS,
        None => false,
    }
}

async fn share_record_failure(code: &str) {
    let mut map = SHARE_FAILED.lock().await;
    let entry = map.entry(code.to_string()).or_insert((0, Instant::now()));
    if entry.1.elapsed().as_secs() >= SHARE_LOCKOUT_SECS {
        // 锁定期已过：重新计数
        *entry = (1, Instant::now());
    } else {
        entry.0 += 1;
    }
}

async fn share_clear_failures(code: &str) {
    SHARE_FAILED.lock().await.remove(code);
}

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
            Some(
                tokio::task::spawn_blocking(move || hash_password(&pwd))
                    .await
                    .map_err(|_| AppError::internal("服务内部错误"))?
                    .map_err(AppError::internal)?,
            )
        }
        _ => None,
    };
    let has_pwd = if password_hash.is_some() { 1 } else { 0 };

    let expires_at = payload.expire_hours.map(|h| {
        chrono::Utc::now()
            .checked_add_signed(chrono::Duration::hours(h as i64))
            .unwrap_or_else(|| chrono::Utc::now() + chrono::Duration::hours(24))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()
    });

    sqlx::query(
        "INSERT INTO shares (code, file_path, is_dir, username, expires_at, password, has_password) VALUES (?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&share_code).bind(&payload.file_path).bind(is_dir_val).bind(&session.username)
    .bind(&expires_at).bind(&password_hash).bind(has_pwd).execute(&pool).await?;

    let _ = log_audit(
        &pool,
        &session.username,
        "create_share",
        Some(&payload.file_path),
        None,
        None,
        None,
    )
    .await;
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
        .bind(&payload.code)
        .bind(&session.username)
        .execute(&pool)
        .await?;
    let _ = log_audit(
        &pool,
        &session.username,
        "delete_share",
        Some(&payload.code),
        None,
        None,
        None,
    )
    .await;
    Ok((StatusCode::OK, "分享链条已切断"))
}

// --- 19. 外链匿名下载（分享页下载按钮的真实目标；附件下载 + 强制下载类型，防同源 XSS）---
pub async fn access_share(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Path(code): Path<String>,
    Query(params): Query<AccessShareRequest>,
    MaybePeer(peer_ip): MaybePeer,
    headers: HeaderMap,
) -> impl IntoResponse {
    use axum::http::header;

    // 匿名端点限速：每 IP 每分钟 10 次（含 Argon2 校验，防 CPU DoS 与无限速爆破）
    let rate_key = extract_ip(peer_ip, &headers).unwrap_or_else(|| format!("share:{}", code));
    if !rate_limit::check_rate_limit(&rate_key, 10, std::time::Duration::from_secs(60)).await {
        return (StatusCode::TOO_MANY_REQUESTS, "请求过于频繁，请稍后再试").into_response();
    }

    let share =
        sqlx::query("SELECT username, file_path, password, expires_at FROM shares WHERE code = ?")
            .bind(&code)
            .fetch_optional(&pool)
            .await;

    let row = match share {
        Ok(Some(r)) => r,
        _ => return (StatusCode::NOT_FOUND, "分享链接不存在或已失效").into_response(),
    };
    let username: String = row.get("username");
    let file_path_str: String = row.get("file_path");
    let db_password: Option<String> = row.get("password");
    let expires_at: Option<String> = row.get("expires_at");

    // 密码校验（有密码的分享必须验证）
    if let Some(pwd_hash) = db_password {
        // 该分享已被连续错误尝试锁定
        if share_check_locked(&code).await {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                "尝试次数过多，分享已临时锁定",
            )
                .into_response();
        }
        let input_pwd = params.password.unwrap_or_default().to_string();
        let hash_clone = pwd_hash.clone();
        let is_valid =
            tokio::task::spawn_blocking(move || verify_password(&hash_clone, &input_pwd))
                .await
                .unwrap_or(false);
        if !is_valid {
            share_record_failure(&code).await;
            return (StatusCode::UNAUTHORIZED, "密码错误").into_response();
        }
        share_clear_failures(&code).await;
    }

    // 过期校验
    if let Some(expire_str) = expires_at
        && let Ok(expire) = NaiveDateTime::parse_from_str(&expire_str, "%Y-%m-%d %H:%M:%S")
        && Utc::now().naive_utc() > expire
    {
        return (StatusCode::GONE, "分享链接已过期").into_response();
    }

    // 真实路径沙箱解析（分享根即文件本身）
    let base = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let full_path = match crate::handlers::utils::safe_join_sandbox(
        base,
        &format!("{}/{}", username, file_path_str),
    ) {
        Ok(p) => p,
        Err(_) => return (StatusCode::NOT_FOUND, "分享内容不存在").into_response(),
    };
    let rel = full_path
        .strip_prefix(base)
        .unwrap_or(&full_path)
        .to_string_lossy()
        .into_owned();
    let sb = match crate::fsutil::Sandbox::new(crate::constants::UPLOADS_DIR) {
        Ok(s) => s,
        Err(_) => return (StatusCode::NOT_FOUND, "分享内容不存在").into_response(),
    };
    let meta = match sb.metadata(&rel) {
        Ok(m) if m.is_file() => m,
        _ => return (StatusCode::BAD_REQUEST, "仅支持文件分享下载").into_response(),
    };

    // 流式返回：一律 attachment；html/svg/xml 强制 octet-stream（同源 XSS 通道封堵）
    match sb.open(&rel) {
        Ok(file) => {
            let mime = mime_guess::from_path(&full_path).first_or_octet_stream();
            let mime_str = mime.to_string();
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                if crate::handlers::utils::is_force_download_mime(&mime_str) {
                    HeaderValue::from_static("application/octet-stream")
                } else {
                    mime_str
                        .parse()
                        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"))
                },
            );
            if let Ok(disposition) = "attachment".parse::<HeaderValue>() {
                headers.insert(header::CONTENT_DISPOSITION, disposition);
            }
            headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            headers.insert(header::CONTENT_LENGTH, HeaderValue::from(meta.len()));
            let body = Body::from_stream(ReaderStream::new(tokio::fs::File::from_std(file)));
            (StatusCode::OK, headers, body).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "文件读取失败").into_response(),
    }
}

// --- 分享页面入口 ---
use crate::templates::AppTemplate;
use askama::Template;

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
    MaybePeer(peer_ip): MaybePeer,
    headers: HeaderMap,
) -> impl IntoResponse {
    let code = &share_id;
    // 匿名端点限速：每 IP 每分钟 30 次（密码表单提交路径含 Argon2，防 CPU DoS）
    let rate_key = extract_ip(peer_ip, &headers).unwrap_or_else(|| format!("share:{}", code));
    if !rate_limit::check_rate_limit(&rate_key, 30, std::time::Duration::from_secs(60)).await {
        return (StatusCode::TOO_MANY_REQUESTS, "请求过于频繁，请稍后再试").into_response();
    }
    // Look up share info（username 用于解析文件真实路径）
    let share =
        sqlx::query("SELECT file_path, password, expires_at, username FROM shares WHERE code = ?")
            .bind(code)
            .fetch_optional(&pool)
            .await
            .unwrap_or(None);

    let (file_path_str, stored_password, expires_at, owner): (
        String,
        Option<String>,
        Option<String>,
        String,
    ) = match share {
        Some(row) => (
            row.get::<String, _>(0),
            row.get::<Option<String>, _>(1),
            row.get::<Option<String>, _>(2),
            row.get::<String, _>(3),
        ),
        None => return (StatusCode::NOT_FOUND, "分享链接不存在或已失效").into_response(),
    };

    // Check expiry
    if let Some(exp) = &expires_at
        && let Ok(exp_dt) = chrono::NaiveDateTime::parse_from_str(exp, "%Y-%m-%d %H:%M:%S")
        && exp_dt < Utc::now().naive_utc()
    {
        return (StatusCode::GONE, "分享链接已过期").into_response();
    }

    let stored_pwd = stored_password.clone().unwrap_or_default();
    let password_required = !stored_pwd.is_empty();
    let submitted_password = params.password.clone().unwrap_or_default();

    // If password protected and wrong/missing password, show password form
    // Use spawn_blocking for Argon2 verification to avoid blocking async runtime
    let pwd_ok = if password_required {
        // 该分享已被连续错误尝试锁定
        if share_check_locked(code).await {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                "尝试次数过多，分享已临时锁定",
            )
                .into_response();
        }
        let hash = stored_pwd.clone();
        let input = submitted_password.clone();
        let ok = tokio::task::spawn_blocking(move || crate::core::verify_password(&hash, &input))
            .await
            .unwrap_or(false);
        if ok {
            share_clear_failures(code).await;
        } else {
            share_record_failure(code).await;
        }
        ok
    } else {
        true
    };
    if password_required && !pwd_ok {
        return AppTemplate(SharePage {
            share_id: share_id.clone(),
            file_path: file_path_str,
            file_size: String::new(),
            file_count: 0,
            is_dir: false,
            password_required: true,
            password: String::new(),
        })
        .into_response();
    }

    // Check if it's a directory（真实路径 = uploads/{owner}/{file_path}，与 share_subfile 一致）
    let base = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let full_path = match crate::handlers::utils::safe_join_sandbox(
        base,
        &format!("{}/{}", owner, file_path_str),
    ) {
        Ok(p) => p,
        Err(_) => return (StatusCode::NOT_FOUND, "分享内容不存在").into_response(),
    };
    let rel = full_path
        .strip_prefix(base)
        .unwrap_or(&full_path)
        .to_string_lossy()
        .into_owned();
    let sb = match crate::fsutil::Sandbox::new(crate::constants::UPLOADS_DIR) {
        Ok(s) => s,
        Err(_) => return (StatusCode::NOT_FOUND, "分享内容不存在").into_response(),
    };
    let is_dir = sb.metadata(&rel).map(|m| m.is_dir()).unwrap_or(false);
    let file_size;
    let file_count;

    if is_dir {
        file_count = sb.read_dir(&rel).map(|d| d.len()).unwrap_or(0);
        file_size = format!("{} 个项目", file_count);
    } else {
        let len = sb.metadata(&rel).map(|m| m.len()).unwrap_or(0);
        file_size = if len < 1024 {
            format!("{} B", len)
        } else if len < 1024 * 1024 {
            format!("{:.1} KB", len as f64 / 1024.0)
        } else {
            format!("{:.1} MB", len as f64 / 1024.0 / 1024.0)
        };
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
    })
    .into_response()
}

// --- 访问分享下的具体文件或子目录 ---
pub async fn share_subfile(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Path((share_id, file_path)): Path<(String, String)>,
    Query(params): Query<AccessShareRequest>,
    MaybePeer(peer_ip): MaybePeer,
    headers: HeaderMap,
) -> impl IntoResponse {
    use axum::http::header;

    // 匿名端点限速：每 IP 每分钟 30 次（含 Argon2 校验，防 CPU DoS）
    let rate_key = extract_ip(peer_ip, &headers).unwrap_or_else(|| format!("share:{}", share_id));
    if !rate_limit::check_rate_limit(&rate_key, 30, std::time::Duration::from_secs(60)).await {
        return (StatusCode::TOO_MANY_REQUESTS, "请求过于频繁，请稍后再试").into_response();
    }

    // 1. 查询分享信息
    let share_row = sqlx::query(
        "SELECT username, file_path, is_dir, password, expires_at FROM shares WHERE code = ?",
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
        // 该分享已被连续错误尝试锁定
        if share_check_locked(&share_id).await {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                "尝试次数过多，分享已临时锁定",
            )
                .into_response();
        }
        let input_pwd = params.password.unwrap_or_default().to_string();
        let hash_clone = pwd_hash.clone();
        let is_valid =
            tokio::task::spawn_blocking(move || verify_password(&hash_clone, &input_pwd))
                .await
                .unwrap_or(false);
        if !is_valid {
            share_record_failure(&share_id).await;
            return (StatusCode::UNAUTHORIZED, "密码错误").into_response();
        }
        share_clear_failures(&share_id).await;
    }

    // 3. 校验有效期
    if let Some(expire_str) = expires_at
        && let Ok(expire) = NaiveDateTime::parse_from_str(&expire_str, "%Y-%m-%d %H:%M:%S")
        && Utc::now().naive_utc() > expire
    {
        return (StatusCode::GONE, "分享链接已过期").into_response();
    }

    let user_root = std::path::PathBuf::from(crate::constants::UPLOADS_DIR).join(&username);
    let share_base = match safe_join_sandbox(&user_root, &share_root_path) {
        Ok(p) => p,
        Err(_) => return (StatusCode::FORBIDDEN, "访问越界").into_response(),
    };
    let target_path = match safe_join_sandbox(&share_base, &file_path) {
        Ok(p) => p,
        Err(_) => return (StatusCode::FORBIDDEN, "访问越界").into_response(),
    };
    // 相对 uploads 根的 rel（openat2 沙箱原子操作；越界符号链接一律 404）
    let sb = match crate::fsutil::Sandbox::new(crate::constants::UPLOADS_DIR) {
        Ok(s) => s,
        Err(_) => return (StatusCode::FORBIDDEN, "访问越界").into_response(),
    };
    let base = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let target_rel = match target_path.strip_prefix(base) {
        Ok(r) => r.to_string_lossy().into_owned(),
        Err(_) => return (StatusCode::FORBIDDEN, "访问越界").into_response(),
    };

    // 路径前缀检查（组件级，阻止 ../ 穿越；沙箱 Result 已拦截主路径，此检查兜底符号链接场景）
    if !target_path.starts_with(&share_base) {
        return (StatusCode::FORBIDDEN, "访问越界").into_response();
    }
    // 存在性经 openat2 沙箱（越界符号链接 → 不存在 → 404）
    let meta = match sb.metadata(&target_rel) {
        Ok(m) => m,
        Err(_) => return (StatusCode::NOT_FOUND, "文件或目录不存在").into_response(),
    };

    // 4. 如果是目录，返回目录内容 JSON（沙箱内遍历，符号链接条目跳过不暴露）
    if meta.is_dir() {
        let items = list_directory_files(&sb, &target_rel).await;
        return (StatusCode::OK, Json(items)).into_response();
    }

    // 增加下载计数 + 审计（异步，不阻塞响应）
    let share_code = share_id.clone();
    let pool_clone = pool.clone();
    let file = file_path.clone();
    tokio::spawn(async move {
        let _ = sqlx::query("UPDATE shares SET download_count = download_count + 1 WHERE code = ?")
            .bind(&share_code)
            .execute(&pool_clone)
            .await;
        let _ = crate::handlers::log_audit(
            &pool_clone,
            &username,
            "share_download",
            Some(&file),
            None,
            None,
            None,
        )
        .await;
    });

    // 5. 如果是文件，流式返回
    match sb.open(&target_rel) {
        Ok(file) => {
            let mime = mime_guess::from_path(&target_path).first_or_octet_stream();
            let mime_str = mime.to_string();
            let mut headers = HeaderMap::new();
            // 分享文件一律附件下载（内联渲染 html/svg 等可导致同源存储型 XSS）
            headers.insert(
                header::CONTENT_TYPE,
                if crate::handlers::utils::is_force_download_mime(&mime_str) {
                    HeaderValue::from_static("application/octet-stream")
                } else {
                    mime_str
                        .parse()
                        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"))
                },
            );
            if let Ok(disposition) = "attachment".parse::<HeaderValue>() {
                headers.insert(header::CONTENT_DISPOSITION, disposition);
            }
            headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            let body = Body::from_stream(ReaderStream::new(tokio::fs::File::from_std(file)));
            (StatusCode::OK, headers, body).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "文件读取失败").into_response(),
    }
}

// --- 辅助函数：列出目录内容（供分享使用；沙箱内遍历，符号链接不跟随/不暴露）---
async fn list_directory_files(
    sb: &crate::fsutil::Sandbox,
    dir_rel: &str,
) -> Vec<serde_json::Value> {
    let mut items = Vec::new();
    let Ok(entries) = sb.read_dir(dir_rel) else {
        return items;
    };
    for item in entries {
        if item.is_symlink() {
            continue; // 分享场景不暴露符号链接（防越界元数据泄漏）
        }
        let relative_path = if dir_rel.is_empty() {
            item.name.to_string_lossy().into_owned()
        } else {
            format!("{}/{}", dir_rel, item.name.to_string_lossy())
        };
        items.push(serde_json::json!({
            "name": item.name.to_string_lossy(),
            "is_dir": item.is_dir(),
            "size": item.size,
            "path": relative_path,
        }));
    }
    items
}
