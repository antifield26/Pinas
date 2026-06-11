use axum::{
    body::Body,
    extract::{Extension, Path, Query},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Json},
};
use chrono::{Utc, NaiveDateTime};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Row};
use uuid::Uuid;
use argon2::password_hash::{rand_core::OsRng, PasswordHasher, SaltString};
use argon2::Argon2;
use tokio_util::io::ReaderStream;

use crate::handlers::utils::{safe_join_sandbox, verify_password, log_audit};
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
pub async fn create_share(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<CreateShareRequest>,
) -> impl IntoResponse {
    let username = &session.username;
    let share_code = Uuid::new_v4().to_string().chars().take(12).collect::<String>();
    let is_dir_val = if payload.is_dir { 1 } else { 0 };
    
    // 处理密码：如果提供了密码，则计算 Argon2 哈希；否则存储 NULL
    let password_hash = match &payload.password {
        Some(pwd) if !pwd.is_empty() => {
            let salt = SaltString::generate(&mut OsRng);
            match Argon2::default().hash_password(pwd.as_bytes(), &salt) {
                Ok(hash) => Some(hash.to_string()),
                Err(e) => {
                    return (StatusCode::INTERNAL_SERVER_ERROR, format!("密码哈希失败: {}", e)).into_response();
                }
            }
        }
        _ => None,
    };
    let has_pwd = if password_hash.is_some() { 1 } else { 0 };

    let expires_at = payload.expire_hours.map(|h| {
        chrono::Utc::now().checked_add_signed(chrono::Duration::hours(h as i64))
            .unwrap().format("%Y-%m-%d %H:%M:%S").to_string()
    });

    let result = sqlx::query(
        "INSERT INTO shares (code, file_path, is_dir, username, expires_at, password, has_password) VALUES (?, ?, ?, ?, ?, ?, ?)"
    )
        .bind(&share_code)
        .bind(&payload.file_path)
        .bind(is_dir_val)
        .bind(username)
        .bind(&expires_at)
        .bind(&password_hash)
        .bind(has_pwd)
        .execute(&pool)
        .await;

    match result {
        Ok(_) => {
            // 审计日志：创建分享
            let details = format!("code={}, expire={:?}, has_password={}", share_code, expires_at, has_pwd);
            let _ = log_audit(&pool, username, "create_share", Some(&payload.file_path), Some(&details)).await;
            (StatusCode::OK, Json(serde_json::json!({ "code": share_code }))).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("提取外链发生冲突: {}", e)).into_response(),
    }
}

// --- 13. 列出当前用户的全量分享 ---
pub async fn list_shares(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> impl IntoResponse {
    let rows = sqlx::query_as::<_, ShareItem>(
        "SELECT code, file_path, is_dir, username, expires_at, has_password, download_count FROM shares WHERE username = ?"
    )
    .bind(&session.username)
    .fetch_all(&pool)
    .await
    .unwrap_or_default();
    Json(rows)
}

// --- 14. 删除分享外链 ---
pub async fn delete_share(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    let code = payload.get("code").and_then(|v| v.as_str()).unwrap_or_default();
    let _ = sqlx::query("DELETE FROM shares WHERE code = ? AND username = ?")
        .bind(code)
        .bind(&session.username)
        .execute(&pool)
        .await;

    // 审计日志：删除分享
    let _ = log_audit(&pool, &session.username, "delete_share", Some(code), None).await;
    (StatusCode::OK, "分享链条已切断").into_response()
}

// --- 19. 外链匿名提取与子目录探索（简单响应）---
pub async fn access_share(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Path(code): Path<String>,
) -> impl IntoResponse {
    let share_meta = sqlx::query("SELECT file_path, username FROM shares WHERE code = ? AND (expires_at IS NULL OR expires_at > datetime('now'))")
        .bind(&code).fetch_optional(&pool).await.unwrap_or(None);

    if let Some(row) = share_meta {
        let f_path: String = row.get("file_path");
        return (StatusCode::OK, format!("Share accessible: {}", f_path)).into_response();
    }
    (StatusCode::GONE, "外链已失效或过期").into_response()
}

// --- 分享页面入口 ---
pub async fn share_page() -> Html<&'static str> {
    Html(include_str!("../../static/index.html"))
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

    // 2. 校验密码（如果有）
    if let Some(pwd_hash) = db_password {
        let input_pwd = params.password.unwrap_or_default();
        if !verify_password(&pwd_hash, &input_pwd) {
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

    let user_root = std::path::PathBuf::from("uploads").join(&username);
    let share_base = safe_join_sandbox(&user_root, &share_root_path);
    let target_path = safe_join_sandbox(&share_base, &file_path);

    if !target_path.starts_with(&share_base) {
        return (StatusCode::FORBIDDEN, "访问越界").into_response();
    }
    if !target_path.exists() {
        return (StatusCode::NOT_FOUND, "文件或目录不存在").into_response();
    }

    // 4. 如果是目录，返回目录内容 JSON
    if target_path.is_dir() {
        let items = list_directory_files(&target_path, &user_root).await;
        return (StatusCode::OK, Json(items)).into_response();
    }

    // 增加下载计数（异步）
    let share_code = share_id.clone();
    let pool_clone = pool.clone();
    tokio::spawn(async move {
        let _ = sqlx::query("UPDATE shares SET download_count = download_count + 1 WHERE code = ?")
            .bind(&share_code)
            .execute(&pool_clone)
            .await;
    });

    // 5. 如果是文件，流式返回
    match tokio::fs::File::open(&target_path).await {
        Ok(file) => {
            let mime = mime_guess::from_path(&target_path).first_or_octet_stream();
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                mime.to_string().parse().unwrap(),
            );
            headers.insert(
                header::ACCEPT_RANGES,
                "bytes".parse().unwrap(),
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