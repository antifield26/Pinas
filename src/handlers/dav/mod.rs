// ====== WebDAV：入口与分发（P1-1 拆分） ======
// 认证见 auth.rs；各方法实现见 ops.rs
mod auth;
mod ops;

use axum::{
    Extension,
    extract::Request,
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use sqlx::SqlitePool;

use crate::constants::UPLOADS_DIR;
use crate::handlers::utils::safe_join_sandbox;

pub(crate) use auth::dav_auth;
use ops::{delete_item, get_file, lock_response, mkcol, move_or_copy, propfind, put_file};

pub use auth::*;

/// WebDAV PUT 防御上限（路由级 body limit 已在 router.rs 设置）
const DAV_MAX_BODY_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// 根路径处理器：/dav 与 /dav/（精确路由无通配符参数，Path 提取会失败，这里直接传空路径）
pub async fn dav_root_handler(Extension(pool): Extension<SqlitePool>, req: Request) -> Response {
    dav_handler(Extension(pool), axum::extract::Path(String::new()), req).await
}

/// GET/HEAD/PUT/DELETE/PROPFIND/MKCOL/COPY/MOVE/LOCK/UNLOCK 单入口分发
pub async fn dav_handler(
    Extension(pool): Extension<SqlitePool>,
    axum::extract::Path(path): axum::extract::Path<String>,
    req: Request,
) -> Response {
    // OPTIONS 免认证（客户端常先无凭据探测能力）
    if req.method() == Method::OPTIONS {
        return options_response();
    }
    // ConnectInfo 在 &req 持有期外提前取出（&Request 非 Send，跨 await 持引用会使 handler future 非 Send）
    let peer_ip = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip());
    let user = match dav_auth(req.headers().clone(), peer_ip, &pool).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    // 注意：&Request<Body> 非 Send（Body 非 Sync），跨 await 持引用会使 handler future 非 Send。
    // 故先克隆 method 与 headers（HeaderMap: Sync），子函数只接收克隆后的头
    let method = req.method().clone();
    let headers = req.headers().clone();
    // http::Method 无 PROPFIND/MKCOL 等非标准方法常量，按字符串分发
    match method.as_str() {
        "OPTIONS" => options_response(),
        "PROPFIND" => propfind(&pool, &user, &path, &headers).await,
        "GET" | "HEAD" => get_file(&user, &path, &headers, &method).await,
        "PUT" => put_file(&pool, &user, &path, req).await,
        "MKCOL" => mkcol(&pool, &user, &path).await,
        "MOVE" => move_or_copy(&pool, &user, &path, true, &headers).await,
        "COPY" => move_or_copy(&pool, &user, &path, false, &headers).await,
        "DELETE" => delete_item(&pool, &user, &path).await,
        "LOCK" => lock_response(),
        "UNLOCK" => StatusCode::NO_CONTENT.into_response(),
        _ => StatusCode::NOT_IMPLEMENTED.into_response(),
    }
}

// ====== 工具函数 ======

/// 绝对路径解析：tokio::fs 在阻塞池线程执行，相对路径依赖池线程 CWD
/// （历史 ENOENT 竞态根源）。统一在 async 上下文解析绝对路径后再交给 tokio::fs
pub(crate) fn absolute_uploads_path(rel: &std::path::Path) -> std::path::PathBuf {
    std::path::absolute(rel).unwrap_or_else(|_| rel.to_path_buf())
}

/// 拆分相对路径为 (父目录, 名称)；根为空串
pub(crate) fn split_last(rel: &str) -> (String, String) {
    match rel.rfind('/') {
        Some(i) => (rel[..i].to_string(), rel[i + 1..].to_string()),
        None => (String::new(), rel.to_string()),
    }
}

/// 逻辑路径拼接（父为空返回 name）
pub(crate) fn logical_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

/// URL 路径段编码（按字节 %XX，保留 ASCII 字母数字与 -_.~/）
pub(crate) fn encode_path(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for &b in seg.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// XML 转义（含控制字符 → U+FFFD，XML 1.0 非法字符）
pub(crate) fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 => out.push('\u{FFFD}'),
            c => out.push(c),
        }
    }
    out
}

/// HTTP-date 格式化（Last-Modified）
pub(crate) fn http_date(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|t| t.format("%a, %d %b %Y %H:%M:%S GMT").to_string())
        .unwrap_or_else(|| "Thu, 01 Jan 1970 00:00:00 GMT".to_string())
}

/// DB created_at（"YYYY-MM-DD HH:MM:SS"）→ ISO8601
pub(crate) fn creation_date(created_at: &str) -> String {
    chrono::NaiveDateTime::parse_from_str(created_at, "%Y-%m-%d %H:%M:%S")
        .map(|t| format!("{}Z", t.format("%Y-%m-%dT%H:%M:%S")))
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// 解析 Destination 头 → 相对路径（剥 /dav 前缀）；仅接受同源 /dav/ 形式
pub(crate) fn parse_destination(dest: &str) -> Result<String, StatusCode> {
    let uri: axum::http::Uri = dest.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let p = uri.path().trim_start_matches('/');
    let rel = p
        .strip_prefix("dav/")
        .or_else(|| p.strip_prefix("dav"))
        .ok_or(StatusCode::BAD_REQUEST)?;
    Ok(rel.trim_matches('/').to_string())
}

/// 查询配额 (used_mb, quota_mb)
pub(crate) async fn quota_info(pool: &SqlitePool, username: &str) -> (i64, i64) {
    sqlx::query_as::<_, (i64, i64)>("SELECT used_mb, quota_mb FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(pool)
        .await
        .unwrap_or(None)
        .unwrap_or((0, 0))
}

/// 物理路径：uploads/{username}/{rel}（供展示/日志；物理操作一律走 openat2 沙箱）
pub(crate) fn physical_path(username: &str, rel: &str) -> Result<std::path::PathBuf, StatusCode> {
    safe_join_sandbox(
        std::path::Path::new(UPLOADS_DIR),
        &format!("{username}/{rel}"),
    )
    .map_err(|_| StatusCode::NOT_FOUND)
}

/// 沙箱（root = uploads）——openat2 BENEATH 原子解析，符号链接越界一律拒绝
pub(crate) fn user_sandbox() -> std::io::Result<crate::fsutil::Sandbox> {
    crate::fsutil::Sandbox::new(UPLOADS_DIR)
}

// ====== OPTIONS ======

pub(crate) fn options_response() -> Response {
    (
        StatusCode::OK,
        [
            (
                "Allow",
                "OPTIONS, GET, HEAD, PUT, DELETE, PROPFIND, MKCOL, COPY, MOVE, LOCK, UNLOCK",
            ),
            ("DAV", "1, 2"),
            ("Content-Length", "0"),
        ],
        "",
    )
        .into_response()
}
