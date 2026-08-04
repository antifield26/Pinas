// ====== WebDAV 端点 ======
// 解锁全平台同步客户端（Rclone / RaiDrive / 手机文件管理器 / Windows 资源管理器映射）。
// 认证：HTTP Basic（独立于 cookie 会话，60s 成功缓存防每请求 argon2）。
// 路径：/dav/{*path} 映射到当前认证用户的云盘根目录。
// 安全：全部路径过 safe_join_sandbox + validate_name；PUT 流式落盘 temp + 原子 rename；
// DELETE 进回收站（可还原）；配额沿用 upload.rs 预检模式；大请求路由级 5GiB 上限（router.rs）。

use axum::{
    extract::Request,
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    Extension,
};
use base64::Engine as _;
use futures_util::StreamExt;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use crate::constants::{TMP_DIR, UPLOADS_DIR};
use crate::handlers::utils::{safe_join_sandbox, update_user_used_mb, validate_name};

/// WebDAV PUT 防御上限（路由级 body limit 已在 router.rs 设置）
const DAV_MAX_BODY_BYTES: u64 = 5 * 1024 * 1024 * 1024;

// ====== Basic 认证 ======

/// 已认证的 WebDAV 用户（提取器）
pub struct DavUser {
    pub username: String,
    pub is_admin: bool,
}

/// 认证成功缓存（username → (通过, 验证时间)）；仅缓存通过态，失败即时校验
static AUTH_CACHE: LazyLock<Mutex<HashMap<String, (bool, Instant)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"pi_nas\"")],
        "Unauthorized",
    )
        .into_response()
}

/// 手动 Basic 认证（WebDAV 单 handler 内调用）：成功 → DavUser；失败 → 401 响应。
/// 60s 成功缓存防每请求 argon2；角色实时查（权限变更即时生效）。
async fn dav_auth(headers: &HeaderMap, pool: &SqlitePool) -> Result<DavUser, Response> {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(unauthorized)?;
    let (scheme, b64) = auth.split_once(' ').ok_or_else(unauthorized)?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return Err(unauthorized());
    }
    let creds = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or_else(unauthorized)?;
    let (user, pass) = creds.split_once(':').ok_or_else(unauthorized)?;

    // 命中缓存（60s）时跳过 argon2 验证；角色实时查。锁不得跨越 await（MutexGuard 非 Send）
    let cached_ok = {
        let cache = AUTH_CACHE.lock().unwrap();
        cache
            .get(user)
            .map(|(ok, t)| *ok && t.elapsed() < std::time::Duration::from_secs(60))
            .unwrap_or(false)
    };
    if cached_ok {
        let role: Option<String> = sqlx::query_scalar("SELECT role FROM users WHERE username = ?")
            .bind(user)
            .fetch_optional(pool)
            .await
            .unwrap_or(None);
        return Ok(DavUser {
            username: user.to_string(),
            is_admin: role.as_deref() == Some(crate::constants::ROLE_ADMIN),
        });
    }

    let Ok(Some(auth_row)) = crate::db::queries::get_user_auth(pool, user).await else {
        return Err(unauthorized());
    };
    if !crate::core::verify_password(&auth_row.0, pass) {
        return Err(unauthorized());
    }
    if auth_row.2 {
        return Err((
            StatusCode::FORBIDDEN,
            [(header::WWW_AUTHENTICATE, "Basic realm=\"pi_nas\"")],
            "请先通过网页登录修改初始密码",
        )
            .into_response());
    }
    AUTH_CACHE
        .lock()
        .unwrap()
        .insert(user.to_string(), (true, Instant::now()));
    Ok(DavUser {
        username: user.to_string(),
        is_admin: auth_row.1 == crate::constants::ROLE_ADMIN,
    })
}

// ====== 主 handler ======

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
    let user = match dav_auth(req.headers(), &pool).await {
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

/// 拆分相对路径为 (父目录, 名称)；根为空串
fn split_last(rel: &str) -> (String, String) {
    match rel.rfind('/') {
        Some(i) => (rel[..i].to_string(), rel[i + 1..].to_string()),
        None => (String::new(), rel.to_string()),
    }
}

/// 逻辑路径拼接（父为空返回 name）
fn logical_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

/// URL 路径段编码（按字节 %XX，保留 ASCII 字母数字与 -_.~/）
fn encode_path(seg: &str) -> String {
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
fn escape_xml(s: &str) -> String {
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
fn http_date(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|t| t.format("%a, %d %b %Y %H:%M:%S GMT").to_string())
        .unwrap_or_else(|| "Thu, 01 Jan 1970 00:00:00 GMT".to_string())
}

/// DB created_at（"YYYY-MM-DD HH:MM:SS"）→ ISO8601
fn creation_date(created_at: &str) -> String {
    chrono::NaiveDateTime::parse_from_str(created_at, "%Y-%m-%d %H:%M:%S")
        .map(|t| format!("{}Z", t.format("%Y-%m-%dT%H:%M:%S")))
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// 解析 Destination 头 → 相对路径（剥 /dav 前缀）；仅接受同源 /dav/ 形式
fn parse_destination(dest: &str) -> Result<String, StatusCode> {
    let uri: axum::http::Uri = dest.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let p = uri.path().trim_start_matches('/');
    let rel = p
        .strip_prefix("dav/")
        .or_else(|| p.strip_prefix("dav"))
        .ok_or(StatusCode::BAD_REQUEST)?;
    Ok(rel.trim_matches('/').to_string())
}

/// 查询配额 (used_mb, quota_mb)
async fn quota_info(pool: &SqlitePool, username: &str) -> (i64, i64) {
    sqlx::query_as::<_, (i64, i64)>("SELECT used_mb, quota_mb FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(pool)
        .await
        .unwrap_or(None)
        .unwrap_or((0, 0))
}

/// 物理路径：uploads/{username}/{rel}
fn physical_path(username: &str, rel: &str) -> Result<std::path::PathBuf, StatusCode> {
    safe_join_sandbox(
        std::path::Path::new(UPLOADS_DIR),
        &format!("{username}/{rel}"),
    )
    .map_err(|_| StatusCode::NOT_FOUND)
}

// ====== OPTIONS ======

fn options_response() -> Response {
    (
        StatusCode::OK,
        [
            ("Allow", "OPTIONS, GET, HEAD, PUT, DELETE, PROPFIND, MKCOL, COPY, MOVE, LOCK, UNLOCK"),
            ("DAV", "1, 2"),
            ("Content-Length", "0"),
        ],
        "",
    )
        .into_response()
}

// ====== PROPFIND ======

/// PROPFIND：Depth 0/1（infinity → 403）；手写 XML multistatus，全量返回（忽略 propfind body）
async fn propfind(pool: &SqlitePool, user: &DavUser, path: &str, headers: &HeaderMap) -> Response {
    let depth = headers
        .get("Depth")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("1");
    if depth.eq_ignore_ascii_case("infinity") {
        return StatusCode::FORBIDDEN.into_response();
    }
    let deep = !depth.starts_with('0');
    let rel = path.trim_start_matches('/');

    // 目标是否存在（磁盘为准）
    let is_root = rel.is_empty();
    if !is_root {
        let full = match physical_path(&user.username, rel) {
            Ok(p) => p,
            Err(_) => return StatusCode::NOT_FOUND.into_response(),
        };
        if !full.exists() {
            return StatusCode::NOT_FOUND.into_response();
        }
    }

    // 收集条目：(href, name, is_dir, size, mtime, created_at)
    let mut entries: Vec<(String, String, bool, u64, i64, String)> = Vec::new();
    if is_root {
        entries.push((
            "/dav/".to_string(),
            String::new(),
            true,
            0,
            0,
            String::new(),
        ));
        if deep {
            let rows: Vec<(String, String, i64, f64, String)> = sqlx::query_as(
                "SELECT name, parent_path, is_dir, size_mb, created_at FROM files \
                 WHERE username = ? AND parent_path = '' ORDER BY is_dir DESC, name COLLATE NOCASE",
            )
            .bind(&user.username)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
            for (name, parent_path, is_dir, size_mb, created_at) in rows {
                let full = match physical_path(
                    &user.username,
                    &logical_path(&parent_path, &name),
                ) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                if !full.exists() {
                    continue;
                }
                let mtime = std::fs::metadata(&full)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                entries.push((
                    format!("/dav/{}", encode_path(&name)),
                    name,
                    is_dir != 0,
                    if is_dir != 0 {
                        0
                    } else {
                        (size_mb * 1024.0 * 1024.0) as u64
                    },
                    mtime,
                    created_at,
                ));
            }
        }
    } else {
        let (parent, name) = split_last(rel);
        let full = physical_path(&user.username, rel).unwrap();
        let meta = match std::fs::metadata(&full) {
            Ok(m) => m,
            Err(_) => return StatusCode::NOT_FOUND.into_response(),
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let created_at: Option<String> =
            sqlx::query_scalar("SELECT created_at FROM files WHERE username = ? AND name = ? AND parent_path = ?")
                .bind(&user.username)
                .bind(&name)
                .bind(&parent)
                .fetch_optional(pool)
                .await
                .unwrap_or(None);
        entries.push((
            format!("/dav/{}", encode_path(rel)),
            name,
            meta.is_dir(),
            if meta.is_dir() { 0 } else { meta.len() },
            mtime,
            created_at.unwrap_or_default(),
        ));
        if deep && meta.is_dir() {
            let rows: Vec<(String, String, i64, f64, String)> = sqlx::query_as(
                "SELECT name, parent_path, is_dir, size_mb, created_at FROM files \
                 WHERE username = ? AND parent_path = ? ORDER BY is_dir DESC, name COLLATE NOCASE",
            )
            .bind(&user.username)
            .bind(rel)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
            for (name, parent_path, is_dir, size_mb, created_at) in rows {
                let full = match physical_path(&user.username, &logical_path(&parent_path, &name)) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                if !full.exists() {
                    continue;
                }
                let mtime = std::fs::metadata(&full)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                entries.push((
                    format!("/dav/{}", encode_path(&logical_path(&parent_path, &name))),
                    name,
                    is_dir != 0,
                    if is_dir != 0 {
                        0
                    } else {
                        (size_mb * 1024.0 * 1024.0) as u64
                    },
                    mtime,
                    created_at,
                ));
            }
        }
    }

    let (used_mb, quota_mb) = quota_info(pool, &user.username).await;
    let used_bytes = (used_mb as u64).saturating_mul(1024 * 1024);
    let avail_bytes = (quota_mb.saturating_sub(used_mb).max(0) as u64)
        .saturating_mul(1024 * 1024);

    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:multistatus xmlns:D=\"DAV:\">\n");
    for (href, name, is_dir, size, mtime, created_at) in &entries {
        let res_type = if *is_dir {
            "<D:collection/>".to_string()
        } else {
            String::new()
        };
        let ctype = if *is_dir {
            String::new()
        } else {
            mime_guess::from_path(name)
                .first_or_octet_stream()
                .to_string()
        };
        xml.push_str("  <D:response>\n");
        xml.push_str(&format!("    <D:href>{}</D:href>\n", escape_xml(href)));
        xml.push_str("    <D:propstat>\n      <D:prop>\n");
        xml.push_str(&format!(
            "        <D:displayname>{}</D:displayname>\n",
            escape_xml(name)
        ));
        xml.push_str(&format!(
            "        <D:getcontentlength>{}</D:getcontentlength>\n",
            size
        ));
        xml.push_str(&format!(
            "        <D:getcontenttype>{}</D:getcontenttype>\n",
            escape_xml(&ctype)
        ));
        xml.push_str(&format!(
            "        <D:getlastmodified>{}</D:getlastmodified>\n",
            http_date(*mtime)
        ));
        xml.push_str(&format!(
            "        <D:creationdate>{}</D:creationdate>\n",
            creation_date(created_at)
        ));
        xml.push_str(&format!("        <D:resourcetype>{}</D:resourcetype>\n", res_type));
        xml.push_str("      </D:prop>\n      <D:status>HTTP/1.1 200 OK</D:status>\n    </D:propstat>\n");
        xml.push_str("    <D:propstat>\n      <D:prop>\n");
        xml.push_str(&format!(
            "        <D:quota-used-bytes>{}</D:quota-used-bytes>\n",
            used_bytes
        ));
        xml.push_str(&format!(
            "        <D:quota-available-bytes>{}</D:quota-available-bytes>\n",
            avail_bytes
        ));
        xml.push_str("      </D:prop>\n      <D:status>HTTP/1.1 200 OK</D:status>\n    </D:propstat>\n");
        xml.push_str("  </D:response>\n");
    }
    xml.push_str("</D:multistatus>");

    (
        StatusCode::MULTI_STATUS,
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        xml,
    )
        .into_response()
}

// ====== GET / HEAD ======

/// GET/HEAD：全量返回（无 Range 限制，区别于 media_proxy 的 2MB 截断）+ Range 支持
async fn get_file(user: &DavUser, path: &str, headers: &HeaderMap, method: &Method) -> Response {
    let rel = path.trim_start_matches('/');
    let full = match physical_path(&user.username, rel) {
        Ok(p) => p,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    // 注：测试/生产环境相对路径下 tokio::fs 存在 ENOENT 竞态（blocking 池解析），统一用 std::fs 同步操作
    let meta = match std::fs::metadata(&full) {
        Ok(m) if m.is_file() => m,
        Ok(_) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let file_size = meta.len();
    let mime = mime_guess::from_path(&full).first_or_octet_stream();
    let mime_str = mime.to_string();
    let force_download = crate::handlers::utils::is_force_download_mime(&mime_str);
    let content_type = if force_download {
        HeaderValue::from_static("application/octet-stream")
    } else {
        mime_str
            .parse()
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"))
    };

    if file_size == 0 {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, content_type);
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(0u64));
        return (StatusCode::OK, headers, axum::body::Body::empty()).into_response();
    }
    if method == Method::HEAD {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, content_type);
        headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(file_size));
        return (StatusCode::OK, headers).into_response();
    }

    let range_header = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok());
    let (start, end, status) = if let Some(range_value) = range_header {
        if let Some((s, e)) = crate::handlers::media::parse_range(range_value, file_size) {
            (s, e, StatusCode::PARTIAL_CONTENT)
        } else {
            return (
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{file_size}"))],
                "",
            )
                .into_response();
        }
    } else {
        (0, file_size - 1, StatusCode::OK)
    };
    let length = end - start + 1;

    let mut file = match std::fs::File::open(&full) {
        Ok(f) => f,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(start)).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    // std 同步打开 + seek（规避相对路径竞态），再转 tokio File 流式读取（ReaderStream 需 AsyncRead）
    let tokio_file = tokio::fs::File::from_std(file);
    let limited = tokio::io::AsyncReadExt::take(tokio_file, length);
    let body = axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(limited));

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, content_type);
    if force_download
        && let Ok(d) = "attachment".parse::<HeaderValue>()
    {
        headers.insert(header::CONTENT_DISPOSITION, d);
    }
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    if let Ok(lm) = http_date(
        meta.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    )
    .parse::<HeaderValue>()
    {
        headers.insert(header::LAST_MODIFIED, lm);
    }
    if status == StatusCode::PARTIAL_CONTENT {
        let content_range = format!("bytes {start}-{end}/{file_size}");
        headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&content_range)
                .unwrap_or_else(|_| HeaderValue::from_static("bytes */0")),
        );
    }
    (status, headers, body).into_response()
}

// ====== PUT ======

/// PUT：流式落盘 uploads/tmp/dav_{uuid} → 配额预检+复核 → 原子 rename 覆盖 → 登记 files 表
async fn put_file(
    pool: &SqlitePool,
    user: &DavUser,
    path: &str,
    req: Request,
) -> Response {
    let rel = path.trim_start_matches('/');
    let (parent, name) = split_last(rel);
    if validate_name(&name).is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }

    // 父目录物理创建 + DB 目录行补插
    let parent_full = match safe_join_sandbox(
        std::path::Path::new(UPLOADS_DIR),
        &format!("{}/{}", user.username, parent),
    ) {
        Ok(p) => p,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    if std::fs::create_dir_all(&parent_full).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    if crate::handlers::file_ops::ensure_dir_rows(pool, &user.username, &parent).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    // 配额预检（有 Content-Length 时提前拒绝，省 IO）
    let (used_mb, quota_mb) = quota_info(pool, &user.username).await;
    if let Some(cl) = req.headers().get(header::CONTENT_LENGTH)
        && let Some(cl) = cl.to_str().ok().and_then(|s| s.parse::<u64>().ok())
    {
        let need = crate::handlers::utils::bytes_to_mb_ceil(cl);
        if used_mb + need > quota_mb {
            return StatusCode::INSUFFICIENT_STORAGE.into_response();
        }
    }

    // 流式写入 temp（先确保 tmp 目录存在，防御性创建）
    let _ = std::fs::create_dir_all(TMP_DIR);
    let tmp = format!("{TMP_DIR}/dav_{}", uuid::Uuid::new_v4());
    let mut file = match std::fs::File::create(&tmp) {
        Ok(f) => f,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let mut stream = req.into_body().into_data_stream();
    let mut written: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(_) => {
                let _ = std::fs::remove_file(&tmp);
                return StatusCode::BAD_REQUEST.into_response();
            }
        };
        written += chunk.len() as u64;
        if written > DAV_MAX_BODY_BYTES {
            let _ = std::fs::remove_file(&tmp);
            return StatusCode::INSUFFICIENT_STORAGE.into_response();
        }
        if std::io::Write::write_all(&mut file, &chunk).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    if std::io::Write::flush(&mut file).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    drop(file);

    // 配额复核（写盘后按实际字节）
    let need = crate::handlers::utils::bytes_to_mb_ceil(written);
    let (used_mb, quota_mb) = quota_info(pool, &user.username).await;
    if used_mb + need > quota_mb {
        let _ = std::fs::remove_file(&tmp);
        return StatusCode::INSUFFICIENT_STORAGE.into_response();
    }

    // 覆盖语义：目标存在则物理替换 + 删除旧 DB 行
    let target = parent_full.join(&name);
    let existed = target.exists();
    if existed {
        if std::fs::remove_file(&target).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        let _ = sqlx::query("DELETE FROM files WHERE username = ? AND name = ? AND parent_path = ?")
            .bind(&user.username)
            .bind(&name)
            .bind(&parent)
            .execute(pool)
            .await;
    }
    if std::fs::rename(&tmp, &target).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    // 登记 + 配额重算
    let _ = sqlx::query(
        "INSERT INTO files (username, name, parent_path, is_dir, size_mb) VALUES (?, ?, ?, 0, ?)",
    )
    .bind(&user.username)
    .bind(&name)
    .bind(&parent)
    .bind((written as f64 / 1024.0 / 1024.0).ceil())
    .execute(pool)
    .await;
    let _ = update_user_used_mb(pool, &user.username).await;
    let _ = crate::handlers::utils::log_audit(
        pool,
        &user.username,
        "webdav_upload",
        Some(&logical_path(&parent, &name)),
        None,
        None,
        None,
    )
    .await;

    if existed {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::CREATED.into_response()
    }
}

// ====== MKCOL ======

async fn mkcol(pool: &SqlitePool, user: &DavUser, path: &str) -> Response {
    let rel = path.trim_start_matches('/');
    let (parent, name) = split_last(rel);
    if validate_name(&name).is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let full = match physical_path(&user.username, rel) {
        Ok(p) => p,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    if full.exists() {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    // 父目录必须已存在（RFC 4918：否则 409）
    let parent_full = match safe_join_sandbox(
        std::path::Path::new(UPLOADS_DIR),
        &format!("{}/{}", user.username, parent),
    ) {
        Ok(p) => p,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    if !parent_full.exists() {
        return StatusCode::CONFLICT.into_response();
    }
    if std::fs::create_dir(&full).is_err() {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let _ = sqlx::query("INSERT INTO files (username, name, parent_path, is_dir) VALUES (?, ?, ?, 1)")
        .bind(&user.username)
        .bind(&name)
        .bind(&parent)
        .execute(pool)
        .await;
    let _ = crate::handlers::utils::log_audit(
        pool,
        &user.username,
        "webdav_mkcol",
        Some(&logical_path(&parent, &name)),
        None,
        None,
        None,
    )
    .await;
    StatusCode::CREATED.into_response()
}

// ====== MOVE / COPY ======

async fn move_or_copy(
    pool: &SqlitePool,
    user: &DavUser,
    path: &str,
    is_move: bool,
    headers: &HeaderMap,
) -> Response {
    let dest_raw = match headers.get("Destination").and_then(|v| v.to_str().ok()) {
        Some(d) => d,
        None => return StatusCode::BAD_REQUEST.into_response(),
    };
    let dest_rel = match parse_destination(dest_raw) {
        Ok(r) => r,
        Err(s) => return s.into_response(),
    };
    let src_rel = path.trim_start_matches('/');
    let (d_parent, d_name) = split_last(&dest_rel);
    if validate_name(&d_name).is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    // 同位置移动 = 无操作
    if src_rel == dest_rel {
        return StatusCode::NO_CONTENT.into_response();
    }

    // 覆盖语义
    let overwrite = !headers
        .get("Overwrite")
        .map(|v| v.as_bytes() == b"F")
        .unwrap_or(false);
    let dst_full = match physical_path(&user.username, &dest_rel) {
        Ok(p) => p,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    if dst_full.exists() && !overwrite {
        return StatusCode::PRECONDITION_FAILED.into_response();
    }
    // 覆盖目标：先移走旧目标（物理 + DB），操作完成后再清理
    let mut displaced: Option<(std::path::PathBuf, String, String, String)> = None; // (tmp_path, name, parent, logical)
    if dst_full.exists() {
        let tmp = format!("{TMP_DIR}/dav_disp_{}", uuid::Uuid::new_v4());
        if std::fs::rename(&dst_full, &tmp).is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        let _ = sqlx::query("DELETE FROM files WHERE username = ? AND name = ? AND parent_path = ?")
            .bind(&user.username)
            .bind(&d_name)
            .bind(&d_parent)
            .execute(pool)
            .await;
        let child_prefix = if d_parent.is_empty() {
            format!("{d_name}/%")
        } else {
            format!("{d_parent}/{d_name}/%")
        };
        let _ = sqlx::query("DELETE FROM files WHERE username = ? AND parent_path LIKE ?")
            .bind(&user.username)
            .bind(&child_prefix)
            .execute(pool)
            .await;
        let displaced_logical = logical_path(&d_parent, &d_name);
        displaced = Some((tmp.into(), d_name.clone(), d_parent.clone(), displaced_logical));
    }

    let (s_parent, s_name) = split_last(src_rel);
    let result = if is_move {
        // 目标父目录需存在（物理 + 目录行）
        let _ = std::fs::create_dir_all(
            safe_join_sandbox(
                std::path::Path::new(UPLOADS_DIR),
                &format!("{}/{}", user.username, d_parent),
            )
            .unwrap_or_default(),
        );
        let _ = crate::handlers::file_ops::ensure_dir_rows(pool, &user.username, &d_parent).await;
        // move_core 为同名移动；Destination 含新文件名时先移动再改名（rename_core 处理子树）
        if s_parent == d_parent {
            crate::handlers::file_ops::rename_core(pool, &user.username, &s_parent, &s_name, &d_name)
                .await
        } else {
            let moved =
                crate::handlers::file_ops::move_core(pool, &user.username, &s_parent, &d_parent, &s_name)
                    .await;
            match moved {
                Ok(()) if d_name != s_name => {
                    crate::handlers::file_ops::rename_core(
                        pool,
                        &user.username,
                        &d_parent,
                        &s_name,
                        &d_name,
                    )
                    .await
                }
                other => other,
            }
        }
    } else {
        copy_recursive(pool, &user.username, &s_parent, &s_name, &d_parent, &d_name).await
    };

    match result {
        Ok(()) => {
            if let Some((tmp, name, parent, logical)) = displaced {
                let _ = std::fs::remove_dir_all(&tmp);
                let _ = crate::handlers::utils::log_audit(
                    pool,
                    &user.username,
                    "webdav_overwrite",
                    Some(&logical),
                    Some(&format!("{parent}/{name}")),
                    None,
                    None,
                )
                .await;
            }
            let action = if is_move { "webdav_move" } else { "webdav_copy" };
            let _ = crate::handlers::utils::log_audit(
                pool,
                &user.username,
                action,
                Some(&logical_path(&s_parent, &s_name)),
                Some(&dest_rel),
                None,
                None,
            )
            .await;
            let _ = update_user_used_mb(pool, &user.username).await;
            StatusCode::CREATED.into_response()
        }
        Err(_) => {
            // 失败：还原被移走的旧目标
            if let Some((tmp, _, _, _)) = displaced {
                let _ = std::fs::rename(&tmp, &dst_full);
            }
            StatusCode::CONFLICT.into_response()
        }
    }
}

/// COPY 递归：磁盘复制 + DB 登记 + 配额累加预检
async fn copy_recursive(
    pool: &SqlitePool,
    username: &str,
    s_parent: &str,
    s_name: &str,
    d_parent: &str,
    d_name: &str,
) -> Result<(), String> {
    use sqlx::Row;
    let s_prefix = logical_path(s_parent, s_name);
    let d_prefix = logical_path(d_parent, d_name);

    // 收集源：自身 + 子树（按路径深度排序保证父先于子）
    let rows: Vec<(String, String, i64, f64)> = sqlx::query_as(
        "SELECT name, parent_path, is_dir, size_mb FROM files \
         WHERE username = ? AND ((parent_path = ? AND name = ?) OR parent_path LIKE ?) \
         ORDER BY length(parent_path), parent_path",
    )
    .bind(username)
    .bind(s_parent)
    .bind(s_name)
    .bind(format!("{s_prefix}/%"))
    .fetch_all(pool)
    .await
    .map_err(|e| format!("COPY 源查询失败: {e}"))?;

    // 配额预检
    let total_mb: f64 = rows
        .iter()
        .filter(|r| r.2 == 0)
        .map(|r| r.3)
        .sum::<f64>()
        .ceil();
    let (used_mb, quota_mb) = {
        let row = sqlx::query("SELECT used_mb, quota_mb FROM users WHERE username = ?")
            .bind(username)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("配额查询失败: {e}"))?;
        match row {
            Some(r) => (r.get::<i64, _>(0), r.get::<i64, _>(1)),
            None => return Err("用户不存在".to_string()),
        }
    };
    if used_mb + total_mb as i64 > quota_mb {
        return Err("存储空间不足".to_string());
    }

    let base = std::path::Path::new(UPLOADS_DIR);
    for (name, parent_path, is_dir, size_mb) in rows {
        // 根对象（源自身行）：落在目标父目录 d_parent 下、名取 d_name（Destination 可改名）；
        // 子树对象：完整路径替换源前缀
        let is_root = parent_path == s_parent && name == s_name;
        let new_parent = if is_root {
            d_parent.to_string()
        } else {
            parent_path.replacen(&format!("{s_prefix}/"), &format!("{d_prefix}/"), 1)
        };
        let target_name = if is_root { d_name } else { &name };
        let src = safe_join_sandbox(base, &format!("{username}/{parent_path}/{name}"))
            .map_err(|_| "COPY 源路径非法".to_string())?;
        let dst_parent = safe_join_sandbox(base, &format!("{username}/{new_parent}"))
            .map_err(|_| "COPY 目标路径非法".to_string())?;
        std::fs::create_dir_all(&dst_parent)
            .map_err(|e| format!("COPY 创建目录失败: {e}"))?;
        if is_dir != 0 {
            std::fs::create_dir_all(dst_parent.join(target_name))
                .map_err(|e| format!("COPY 创建目录失败: {e}"))?;
            let _ = sqlx::query(
                "INSERT INTO files (username, name, parent_path, is_dir) VALUES (?, ?, ?, 1)",
            )
            .bind(username)
            .bind(target_name)
            .bind(&new_parent)
            .execute(pool)
            .await;
        } else {
            std::fs::copy(&src, dst_parent.join(target_name))
                .map_err(|e| format!("COPY 复制文件失败: {e}"))?;
            let _ = sqlx::query(
                "INSERT INTO files (username, name, parent_path, is_dir, size_mb) VALUES (?, ?, ?, 0, ?)",
            )
            .bind(username)
            .bind(target_name)
            .bind(&new_parent)
            .bind(size_mb)
            .execute(pool)
            .await;
        }
    }
    Ok(())
}

// ====== DELETE ======

async fn delete_item(pool: &SqlitePool, user: &DavUser, path: &str) -> Response {
    let rel = path.trim_start_matches('/');
    if rel.is_empty() {
        return StatusCode::FORBIDDEN.into_response(); // 不允许删除根
    }
    let full = match physical_path(&user.username, rel) {
        Ok(p) => p,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    if !full.exists() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let (parent, name) = split_last(rel);
    // 进回收站（可还原），与网页删除语义一致
    if crate::handlers::file_ops::delete_to_trash(pool, &user.username, &parent, &name)
        .await
        .is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let _ = update_user_used_mb(pool, &user.username).await;
    let _ = crate::handlers::utils::log_audit(
        pool,
        &user.username,
        "webdav_delete",
        Some(&logical_path(&parent, &name)),
        None,
        None,
        None,
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}

// ====== LOCK / UNLOCK（伪实现） ======
// 单写者场景（家庭云盘）：返回空写锁不持有状态，客户端（Windows 资源管理器）依赖 LOCK 成功

fn lock_response() -> Response {
    let token = uuid::Uuid::new_v4();
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <D:prop xmlns:D=\"DAV:\"><D:lockdiscovery><D:activelock>\n\
         <D:locktype><D:write/></D:locktype><D:lockscope><D:exclusive/></D:lockscope>\n\
         <D:depth>infinity</D:depth><D:owner/><D:timeout>Second-3600</D:timeout>\n\
         <D:locktoken><D:href>urn:uuid:{token}</D:href></D:locktoken>\n\
         </D:activelock></D:lockdiscovery></D:prop>"
    );
    (
        StatusCode::OK,
        [
            ("Content-Type", "application/xml; charset=utf-8".to_string()),
            ("Lock-Token", format!("<urn:uuid:{token}>")),
        ],
        body,
    )
        .into_response()
}
