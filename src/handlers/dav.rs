// ====== WebDAV 端点 ======
// 解锁全平台同步客户端（Rclone / RaiDrive / 手机文件管理器 / Windows 资源管理器映射）。
// 认证：HTTP Basic（独立于 cookie 会话，60s 成功缓存防每请求 argon2）。
// 路径：/dav/{*path} 映射到当前认证用户的云盘根目录。
// 安全：全部路径过 safe_join_sandbox + validate_name；PUT 流式落盘 temp + 原子 rename；
// DELETE 进回收站（可还原）；配额沿用 upload.rs 预检模式；大请求路由级 5GiB 上限（router.rs）。

use axum::{
    Extension,
    extract::Request,
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
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

/// 认证成功缓存：username → (凭证指纹, 验证时间)。仅缓存通过态，失败即时校验。
/// 指纹 = sha256(username\0password)——命中必须指纹一致，绝不允许仅凭用户名放行
/// （历史缺陷：60s 窗口内任意密码可通过认证，C1 修复）。
static AUTH_CACHE: LazyLock<Mutex<HashMap<String, (String, Instant)>>> =
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
/// 60s 成功缓存防每请求 argon2（键值含凭证指纹，见 AUTH_CACHE）；角色实时查（权限变更即时生效）。
/// 未命中缓存时先过限速：Argon2 ~100ms 是公网可用的 CPU DoS 面，必须按对端 IP 限频。
async fn dav_auth(
    headers: HeaderMap,
    peer_ip: Option<std::net::IpAddr>,
    pool: &SqlitePool,
) -> Result<DavUser, Response> {
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

    // 命中缓存（60s）时跳过 argon2 验证；指纹必须与本次提交的凭证完全一致。角色实时查。
    // 锁不得跨越 await（MutexGuard 非 Send）
    // 中毒恢复：panic=abort 下 Mutex 中毒的 unwrap 会把整站打死，必须 into_inner 恢复
    let cred_fp = crate::core::hash_token(&format!("{user}\u{0}{pass}"));
    let cached_ok = {
        let cache = AUTH_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .get(user)
            .map(|(fp, t)| *fp == cred_fp && t.elapsed() < std::time::Duration::from_secs(60))
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

    // 限速：仅未命中缓存（即必须跑 argon2）的尝试计数——成功路径 60s 内走缓存不再消耗额度。
    // 键与登录限速同源（回环信任 CF-Connecting-IP，直连用真实对端 IP）
    let rate_key = crate::handlers::auth::extract_ip(peer_ip, &headers)
        .unwrap_or_else(|| format!("dav:user:{user}"));
    if !crate::handlers::rate_limit::check_rate_limit(
        &rate_key,
        crate::constants::LOGIN_RATE_LIMIT_ATTEMPTS,
        std::time::Duration::from_secs(crate::constants::LOGIN_RATE_LIMIT_WINDOW_SECS),
    )
    .await
    {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            [(header::WWW_AUTHENTICATE, "Basic realm=\"pi_nas\"")],
            "请求过于频繁，请稍后再试",
        )
            .into_response());
    }

    // 用户不存在时用哑哈希等时校验（抹平用户枚举时序侧信道）
    let auth_row = match crate::db::queries::get_user_auth(pool, user).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let dummy_pass = pass.to_string();
            let _ = tokio::task::spawn_blocking(move || {
                crate::core::verify_password(
                    crate::handlers::auth::dummy_hash_for_timing(),
                    &dummy_pass,
                )
            })
            .await
            .unwrap_or(false);
            return Err(unauthorized());
        }
        Err(_) => return Err(unauthorized()),
    };
    // Argon2 为阻塞 CPU 操作，必须移出 async 运行时（同 auth.rs 登录路径）
    let pass2 = pass.to_string();
    let hash2 = auth_row.0.clone();
    let ok = tokio::task::spawn_blocking(move || crate::core::verify_password(&hash2, &pass2))
        .await
        .unwrap_or(false);
    if !ok {
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
        .unwrap_or_else(|e| e.into_inner())
        .insert(user.to_string(), (cred_fp, Instant::now()));
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
fn absolute_uploads_path(rel: &std::path::Path) -> std::path::PathBuf {
    std::path::absolute(rel).unwrap_or_else(|_| rel.to_path_buf())
}

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
        if !tokio::fs::try_exists(&absolute_uploads_path(&full))
            .await
            .unwrap_or(false)
        {
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
                let full = match physical_path(&user.username, &logical_path(&parent_path, &name)) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                if !tokio::fs::try_exists(&absolute_uploads_path(&full))
                    .await
                    .unwrap_or(false)
                {
                    continue;
                }
                let mtime = tokio::fs::metadata(&absolute_uploads_path(&full))
                    .await
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
        let meta = match tokio::fs::metadata(&absolute_uploads_path(&full)).await {
            Ok(m) => m,
            Err(_) => return StatusCode::NOT_FOUND.into_response(),
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let created_at: Option<String> = sqlx::query_scalar(
            "SELECT created_at FROM files WHERE username = ? AND name = ? AND parent_path = ?",
        )
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
                if !tokio::fs::try_exists(&absolute_uploads_path(&full))
                    .await
                    .unwrap_or(false)
                {
                    continue;
                }
                let mtime = tokio::fs::metadata(&absolute_uploads_path(&full))
                    .await
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
    let avail_bytes = (quota_mb.saturating_sub(used_mb).max(0) as u64).saturating_mul(1024 * 1024);

    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:multistatus xmlns:D=\"DAV:\">\n",
    );
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
        xml.push_str(&format!(
            "        <D:resourcetype>{}</D:resourcetype>\n",
            res_type
        ));
        xml.push_str(
            "      </D:prop>\n      <D:status>HTTP/1.1 200 OK</D:status>\n    </D:propstat>\n",
        );
        xml.push_str("    <D:propstat>\n      <D:prop>\n");
        xml.push_str(&format!(
            "        <D:quota-used-bytes>{}</D:quota-used-bytes>\n",
            used_bytes
        ));
        xml.push_str(&format!(
            "        <D:quota-available-bytes>{}</D:quota-available-bytes>\n",
            avail_bytes
        ));
        xml.push_str(
            "      </D:prop>\n      <D:status>HTTP/1.1 200 OK</D:status>\n    </D:propstat>\n",
        );
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

    let range_header = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
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
    if force_download && let Ok(d) = "attachment".parse::<HeaderValue>() {
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
async fn put_file(pool: &SqlitePool, user: &DavUser, path: &str, req: Request) -> Response {
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
    if crate::handlers::file_ops::ensure_dir_rows(pool, &user.username, &parent)
        .await
        .is_err()
    {
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
    // 全程 tokio::fs + 绝对路径：std::fs 阻塞写会在 async worker 上卡住整条上传
    // （1GB 慢速上行可占死一个 worker，3-4 并发冻结全站）
    let _ = std::fs::create_dir_all(TMP_DIR);
    let tmp = absolute_uploads_path(std::path::Path::new(&format!(
        "{TMP_DIR}/dav_{}",
        uuid::Uuid::new_v4()
    )));
    let mut file = match tokio::fs::File::create(&tmp).await {
        Ok(f) => f,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let mut stream = req.into_body().into_data_stream();
    let mut written: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(_) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                return StatusCode::BAD_REQUEST.into_response();
            }
        };
        written += chunk.len() as u64;
        if written > DAV_MAX_BODY_BYTES {
            let _ = tokio::fs::remove_file(&tmp).await;
            return StatusCode::INSUFFICIENT_STORAGE.into_response();
        }
        use tokio::io::AsyncWriteExt;
        if file.write_all(&chunk).await.is_err() {
            let _ = tokio::fs::remove_file(&tmp).await;
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    use tokio::io::AsyncWriteExt;
    if file.flush().await.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    drop(file);

    // 内容策略前置（与分片上传一致）：扩展名黑名单 + 完整文件 MIME 检测（防 WebDAV 绕过）。
    // 必须在触碰目标文件之前对 tmp 完成——历史上 rename 之后才校验，覆盖场景策略失败时
    // 会连删新旧两个文件，旧文件永久丢失
    if crate::handlers::utils::is_blocked_extension(&name) {
        let _ = tokio::fs::remove_file(&tmp).await;
        return StatusCode::FORBIDDEN.into_response();
    }
    if !crate::handlers::utils::is_allowed_mime_streaming(&tmp)
        .await
        .unwrap_or(true)
    {
        let _ = tokio::fs::remove_file(&tmp).await;
        return StatusCode::FORBIDDEN.into_response();
    }

    // 覆盖语义：rename(2) 对同文件系统原子替换，绝不先删旧文件再改名
    // （先删后改存在崩溃/失败窗口：旧文件已删、新文件未就位 = 永久数据丢失）
    let target = absolute_uploads_path(&parent_full.join(&name));
    let existed = tokio::fs::try_exists(&target).await.unwrap_or(false);

    // 配额复核（写盘后按实际字节；覆盖时按增量——旧文件大小将被释放，不算重复占用）
    let mut need = crate::handlers::utils::bytes_to_mb_ceil(written) as f64;
    let (used_mb, quota_mb) = quota_info(pool, &user.username).await;
    if existed {
        let old_mb: f64 = sqlx::query_scalar(
            "SELECT size_mb FROM files WHERE username = ? AND name = ? AND parent_path = ?",
        )
        .bind(&user.username)
        .bind(&name)
        .bind(&parent)
        .fetch_optional(pool)
        .await
        .unwrap_or(None)
        .unwrap_or(0.0);
        need = (need - old_mb).max(0.0);
    }
    if used_mb as f64 + need > quota_mb as f64 {
        let _ = tokio::fs::remove_file(&tmp).await;
        return StatusCode::INSUFFICIENT_STORAGE.into_response();
    }
    if tokio::fs::rename(&tmp, &target).await.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    // 登记：覆盖走 UPDATE（保留行元数据），新建走 INSERT；配额随后全量重算
    let size_mb = (written as f64 / 1024.0 / 1024.0).ceil();
    if existed {
        let _ = sqlx::query(
            "UPDATE files SET size_mb = ?, identifier = NULL WHERE username = ? AND name = ? AND parent_path = ?",
        )
        .bind(size_mb)
        .bind(&user.username)
        .bind(&name)
        .bind(&parent)
        .execute(pool)
        .await;
    } else {
        let _ = sqlx::query(
            "INSERT INTO files (username, name, parent_path, is_dir, size_mb) VALUES (?, ?, ?, 0, ?)",
        )
        .bind(&user.username)
        .bind(&name)
        .bind(&parent)
        .bind(size_mb)
        .execute(pool)
        .await;
    }
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
    let _ =
        sqlx::query("INSERT INTO files (username, name, parent_path, is_dir) VALUES (?, ?, ?, 1)")
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
    // 覆盖目标：先移走旧目标（物理 + DB 行暂存），移动成功后再清理。
    // DB 行暂存是为了失败时能完整恢复——旧实现删行后失败只还原物理文件，
    // 目标文件从此"隐身"（无 DB 行 → UI 不可见且会被 ensure_file_on_disk 判为孤儿）
    #[derive(sqlx::FromRow)]
    struct DisplacedFileRow {
        name: String,
        parent_path: String,
        is_dir: i64,
        size_mb: f64,
        identifier: Option<String>,
    }
    // (位移文件, 元数据文件, name, parent, logical, 暂存的 DB 行)
    let mut displaced: Option<(
        std::path::PathBuf,
        std::path::PathBuf,
        String,
        String,
        String,
        Vec<DisplacedFileRow>,
    )> = None;
    if dst_full.exists() {
        // M9：位移目标放在 uploads/.dav_disp（不在 TMP_DIR 内——24h 临时清扫不得触碰），
        // 并落一份 JSON 元数据（目标路径 + 暂存 DB 行），崩溃后由启动任务 recover_dav_disp 还原
        let disp_uuid = uuid::Uuid::new_v4().to_string();
        let disp_dir = std::path::Path::new(crate::constants::DAV_DISP_DIR);
        let _ = std::fs::create_dir_all(disp_dir);
        let disp_path = disp_dir.join(format!("{disp_uuid}.d"));
        let meta_path = disp_dir.join(format!("{disp_uuid}.json"));
        if std::fs::rename(&dst_full, &disp_path).is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        let child_prefix = if d_parent.is_empty() {
            format!("{}/%", crate::db::queries::escape_like(&d_name))
        } else {
            format!(
                "{}/{}/%",
                crate::db::queries::escape_like(&d_parent),
                crate::db::queries::escape_like(&d_name)
            )
        };
        // 暂存目标及其子树的 DB 行（失败回滚时重插）
        let rows: Vec<DisplacedFileRow> = sqlx::query_as(
            "SELECT name, parent_path, is_dir, size_mb, identifier FROM files WHERE username = ? AND ((name = ? AND parent_path = ?) OR parent_path LIKE ? ESCAPE '\\')",
        )
        .bind(&user.username)
        .bind(&d_name)
        .bind(&d_parent)
        .bind(&child_prefix)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
        let _ =
            sqlx::query("DELETE FROM files WHERE username = ? AND name = ? AND parent_path = ?")
                .bind(&user.username)
                .bind(&d_name)
                .bind(&d_parent)
                .execute(pool)
                .await;
        let _ =
            sqlx::query("DELETE FROM files WHERE username = ? AND parent_path LIKE ? ESCAPE '\\'")
                .bind(&user.username)
                .bind(&child_prefix)
                .execute(pool)
                .await;
        let meta = serde_json::json!({
            "username": user.username,
            "parent": d_parent,
            "name": d_name,
            "rows": rows.iter().map(|r| serde_json::json!({
                "name": r.name,
                "parent_path": r.parent_path,
                "is_dir": r.is_dir,
                "size_mb": r.size_mb,
                "identifier": r.identifier,
            })).collect::<Vec<_>>(),
        });
        let _ = std::fs::write(&meta_path, meta.to_string());
        let displaced_logical = logical_path(&d_parent, &d_name);
        displaced = Some((
            disp_path,
            meta_path,
            d_name.clone(),
            d_parent.clone(),
            displaced_logical,
            rows,
        ));
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
            crate::handlers::file_ops::rename_core(
                pool,
                &user.username,
                &s_parent,
                &s_name,
                &d_name,
            )
            .await
        } else {
            let moved = crate::handlers::file_ops::move_core(
                pool,
                &user.username,
                &s_parent,
                &d_parent,
                &s_name,
            )
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
        copy_recursive(pool, &user.username, &s_parent, &s_name, &d_parent, &d_name)
            .await
            .map_err(|e| crate::error::AppError::internal_log("WebDAV 复制", e))
    };

    match result {
        Ok(()) => {
            if let Some((tmp, meta_path, name, parent, logical, _rows)) = displaced {
                // 成功：DB 行已删，物理位移文件 + 元数据清掉
                let _ = std::fs::remove_dir_all(&tmp);
                let _ = std::fs::remove_file(&meta_path);
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
            let action = if is_move {
                "webdav_move"
            } else {
                "webdav_copy"
            };
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
            // 失败：完整还原被移走的旧目标（物理文件 + DB 行）+ 清理元数据
            if let Some((tmp, meta_path, _, _, _, rows)) = displaced {
                let _ = std::fs::rename(&tmp, &dst_full);
                for r in rows {
                    let _ = sqlx::query(
                        "INSERT OR IGNORE INTO files (username, name, parent_path, is_dir, size_mb, identifier) VALUES (?, ?, ?, ?, ?, ?)",
                    )
                    .bind(&user.username)
                    .bind(&r.name)
                    .bind(&r.parent_path)
                    .bind(r.is_dir)
                    .bind(r.size_mb)
                    .bind(&r.identifier)
                    .execute(pool)
                    .await;
                }
                let _ = std::fs::remove_file(&meta_path);
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

    let total_mb: f64 = rows
        .iter()
        .filter(|r| r.2 == 0)
        .map(|r| r.3)
        .sum::<f64>()
        .ceil();

    // 阶段一：磁盘复制（无任何 DB 写入）。std::fs::copy 是阻塞 IO，必须移出 async worker
    // 且移出写事务——历史实现把整个拷贝过程包在 SQLite 写事务里：大目录 COPY 会
    // 占死一个 tokio worker + 持写锁数分钟，全站写操作 busy_timeout 后 500。
    let base = std::path::Path::new(UPLOADS_DIR);
    let mut disk_plan: Vec<(
        std::path::PathBuf,
        std::path::PathBuf,
        bool,
        String,
        String,
        f64,
    )> = Vec::with_capacity(rows.len());
    for (name, parent_path, is_dir, size_mb) in &rows {
        // 根对象（源自身行）：落在目标父目录 d_parent 下、名取 d_name（Destination 可改名）；
        // 子树对象：完整路径替换源前缀
        let is_root = parent_path == s_parent && name == s_name;
        let new_parent = if is_root {
            d_parent.to_string()
        } else {
            parent_path.replacen(&format!("{s_prefix}/"), &format!("{d_prefix}/"), 1)
        };
        let target_name = if is_root { d_name } else { name };
        let src = safe_join_sandbox(base, &format!("{username}/{parent_path}/{name}"))
            .map_err(|_| "COPY 源路径非法".to_string())?;
        let dst_parent = safe_join_sandbox(base, &format!("{username}/{new_parent}"))
            .map_err(|_| "COPY 目标路径非法".to_string())?;
        std::fs::create_dir_all(&dst_parent).map_err(|e| format!("COPY 创建目录失败: {e}"))?;
        disk_plan.push((
            src,
            dst_parent.join(target_name),
            *is_dir != 0,
            new_parent,
            target_name.to_string(),
            *size_mb,
        ));
    }
    for (src, dst, is_dir, _new_parent, _target_name, _size_mb) in &disk_plan {
        if *is_dir {
            std::fs::create_dir_all(dst).map_err(|e| format!("COPY 创建目录失败: {e}"))?;
        } else {
            let src = src.clone();
            let dst = dst.clone();
            tokio::task::spawn_blocking(move || std::fs::copy(&src, &dst))
                .await
                .map_err(|_e| "COPY 复制任务失败".to_string())?
                .map_err(|e| format!("COPY 复制文件失败: {e}"))?;
        }
    }

    // 阶段二：短事务（配额复核 + 登记，无阻塞 IO）。并发 COPY 之间由 SQLite 写锁串行化，
    // 避免预检通过后互相穿插导致配额超额；失败回滚不留半截 DB 行，并尽力清理已复制文件
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("COPY 事务失败: {e}"))?;
    let (used_mb, quota_mb) = {
        let row = sqlx::query("SELECT used_mb, quota_mb FROM users WHERE username = ?")
            .bind(username)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| format!("配额查询失败: {e}"))?;
        match row {
            Some(r) => (r.get::<i64, _>(0), r.get::<i64, _>(1)),
            None => return Err("用户不存在".to_string()),
        }
    };
    if used_mb + total_mb as i64 > quota_mb {
        drop(tx);
        // 仅当目标根路径能安全解析时才清理（沙箱失败绝不触碰任何目录）
        if let Ok(root) = safe_join_sandbox(base, &format!("{username}/{d_parent}/{d_name}")) {
            let _ = std::fs::remove_dir_all(&root);
        }
        return Err("存储空间不足".to_string());
    }

    for (_src, _dst, is_dir, new_parent, target_name, size_mb) in &disk_plan {
        if *is_dir {
            let _ = sqlx::query(
                "INSERT INTO files (username, name, parent_path, is_dir) VALUES (?, ?, ?, 1)",
            )
            .bind(username)
            .bind(target_name)
            .bind(new_parent)
            .execute(&mut *tx)
            .await;
        } else {
            let _ = sqlx::query(
                "INSERT INTO files (username, name, parent_path, is_dir, size_mb) VALUES (?, ?, ?, 0, ?)",
            )
            .bind(username)
            .bind(target_name)
            .bind(new_parent)
            .bind(size_mb)
            .execute(&mut *tx)
            .await;
        }
    }
    let _ = sqlx::query("UPDATE users SET used_mb = MAX(0, used_mb + ?) WHERE username = ?")
        .bind(total_mb as i64)
        .bind(username)
        .execute(&mut *tx)
        .await;
    tx.commit()
        .await
        .map_err(|e| format!("COPY 提交失败: {e}"))?;
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
