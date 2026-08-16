// ====== WebDAV：方法实现（P1-1 拆分） ======
use axum::{
    extract::Request,
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use sqlx::SqlitePool;

use crate::constants::{TMP_DIR, UPLOADS_DIR};
use crate::handlers::dav::DAV_MAX_BODY_BYTES;
use crate::handlers::utils::{update_user_used_mb, validate_name, validate_rel_path};

use super::auth::DavUser;
use crate::handlers::dav::{
    absolute_uploads_path, creation_date, encode_path, escape_xml, http_date, logical_path,
    parse_destination, physical_path, quota_info, split_last, user_sandbox,
};

// ====== PROPFIND ======

/// PROPFIND：Depth 0/1（infinity → 403）；手写 XML multistatus，全量返回（忽略 propfind body）
pub(crate) async fn propfind(
    pool: &SqlitePool,
    user: &DavUser,
    path: &str,
    headers: &HeaderMap,
) -> Response {
    let depth = headers
        .get("Depth")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("1");
    if depth.eq_ignore_ascii_case("infinity") {
        return StatusCode::FORBIDDEN.into_response();
    }
    let deep = !depth.starts_with('0');
    let rel = path.trim_start_matches('/');
    let sb = match user_sandbox() {
        Ok(s) => s,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    // 目标是否存在（磁盘为准；openat2 沙箱判定，越界符号链接视为不存在）
    let is_root = rel.is_empty();
    if !is_root {
        let full_rel = format!("{}/{}", user.username, rel);
        // P2-17：try_exists 仅把 NotFound 归为“不存在”，其余 IO 异常是真实故障——
        // 记警告后按不存在降级（对外 404 语义不变）
        match sb.try_exists(&full_rel) {
            Ok(true) => {}
            Ok(false) => return StatusCode::NOT_FOUND.into_response(),
            Err(e) => {
                tracing::warn!(
                    "[DAV] PROPFIND 目标存在性检查异常: path={} err={}",
                    full_rel,
                    e
                );
                return StatusCode::NOT_FOUND.into_response();
            }
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
            let rows: Vec<(String, String, i64, f64, String)> = match sqlx::query_as(
                "SELECT name, parent_path, is_dir, size_mb, created_at FROM files \
                 WHERE username = ? AND parent_path = '' ORDER BY is_dir DESC, name COLLATE NOCASE",
            )
            .bind(&user.username)
            .fetch_all(pool)
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        "[DAV] PROPFIND 根目录查询失败，返回空列表: username={} err={}",
                        user.username,
                        e
                    );
                    Vec::new()
                }
            };
            for (name, parent_path, is_dir, _size_mb, created_at) in rows {
                let full_rel = format!("{}/{}", user.username, logical_path(&parent_path, &name));
                match sb.try_exists(&full_rel) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(e) => {
                        tracing::warn!(
                            "[DAV] PROPFIND 条目存在性检查异常，跳过: path={} err={}",
                            full_rel,
                            e
                        );
                        continue;
                    }
                }
                let meta = sb.metadata(&full_rel).ok();
                let mtime = meta
                    .as_ref()
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
                        // M6：getcontentlength 用真实字节——size_mb 反算被向上取整放大最多 ~1MiB，
                        // rclone 等客户端据此校验会读到 EOF 后仍等待
                        meta.as_ref().map(|m| m.len()).unwrap_or(0)
                    },
                    mtime,
                    created_at,
                ));
            }
        }
    } else {
        let (parent, name) = split_last(rel);
        let full_rel = format!("{}/{}", user.username, rel);
        let meta = match sb.metadata(&full_rel) {
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
            let rows: Vec<(String, String, i64, f64, String)> = match sqlx::query_as(
                "SELECT name, parent_path, is_dir, size_mb, created_at FROM files \
                 WHERE username = ? AND parent_path = ? ORDER BY is_dir DESC, name COLLATE NOCASE",
            )
            .bind(&user.username)
            .bind(rel)
            .fetch_all(pool)
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        "[DAV] PROPFIND 子目录查询失败，返回空列表: username={} parent={} err={}",
                        user.username,
                        rel,
                        e
                    );
                    Vec::new()
                }
            };
            for (name, parent_path, is_dir, _size_mb, created_at) in rows {
                let full_rel = format!("{}/{}", user.username, logical_path(&parent_path, &name));
                match sb.try_exists(&full_rel) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(e) => {
                        tracing::warn!(
                            "[DAV] PROPFIND 条目存在性检查异常，跳过: path={} err={}",
                            full_rel,
                            e
                        );
                        continue;
                    }
                }
                let meta = sb.metadata(&full_rel).ok();
                let mtime = meta
                    .as_ref()
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
                        // 同 M6：真实字节，非 size_mb 反算
                        meta.as_ref().map(|m| m.len()).unwrap_or(0)
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
pub(crate) async fn get_file(
    user: &DavUser,
    path: &str,
    headers: &HeaderMap,
    method: &Method,
) -> Response {
    let rel = path.trim_start_matches('/');
    let full_rel = format!("{}/{}", user.username, rel);
    let full = match physical_path(&user.username, rel) {
        Ok(p) => p,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let sb = match user_sandbox() {
        Ok(s) => s,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    // openat2 沙箱元数据 + 打开（越界符号链接 → 404）
    let meta = match sb.metadata(&full_rel) {
        Ok(m) if m.is_file() => m,
        _ => return StatusCode::NOT_FOUND.into_response(),
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

    let mut file = match sb.open(&full_rel) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return StatusCode::NOT_FOUND.into_response();
        }
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
pub(crate) async fn put_file(
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

    // 父目录物理创建（openat2 沙箱内逐级 mkdir）+ DB 目录行补插
    let parent_rel = format!("{}/{}", user.username, parent);
    let sb = match user_sandbox() {
        Ok(s) => s,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if let Err(e) = sb.create_dir_all(&parent_rel) {
        tracing::error!("[DAV] PUT 创建父目录失败: path={} err={}", parent_rel, e);
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    if let Err(e) = crate::handlers::file_ops::ensure_dir_rows(pool, &user.username, &parent).await
    {
        tracing::error!("[DAV] PUT 补插目录行失败: parent={} err={}", parent, e);
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
    let tmp_uuid = format!("{TMP_DIR}/dav_{}", uuid::Uuid::new_v4());
    // 相对 uploads 根的 rel（沙箱 root = uploads）：uploads/tmp → tmp
    let tmp_rel = tmp_uuid.trim_start_matches("uploads/").to_string();
    let tmp = absolute_uploads_path(std::path::Path::new(&tmp_uuid));
    let mut file = match tokio::fs::File::create(&tmp).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("[DAV] PUT 创建临时文件失败: path={} err={}", tmp_uuid, e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
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
        if let Err(e) = file.write_all(&chunk).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            tracing::error!("[DAV] PUT 写入临时文件失败: path={} err={}", tmp_uuid, e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    }
    use tokio::io::AsyncWriteExt;
    if let Err(e) = file.flush().await {
        let _ = tokio::fs::remove_file(&tmp).await;
        tracing::error!("[DAV] PUT 刷盘失败: path={} err={}", tmp_uuid, e);
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
    // P2-17：MIME 检测失败（IO 异常）不阻断写入以保持历史降级语义，但须记告警——
    // 检测被静默跳过意味着内容策略可能被绕过而不自知
    match crate::handlers::utils::is_allowed_mime_streaming(&tmp).await {
        Ok(true) => {}
        Ok(false) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return StatusCode::FORBIDDEN.into_response();
        }
        Err(e) => {
            tracing::warn!(
                "[DAV] PUT MIME 检测异常，跳过内容策略: tmp={} err={}",
                tmp_uuid,
                e
            );
        }
    }

    // 覆盖语义：rename(2) 对同文件系统原子替换，绝不先删旧文件再改名
    // （先删后改存在崩溃/失败窗口：旧文件已删、新文件未就位 = 永久数据丢失）
    let target_rel = if parent.is_empty() {
        format!("{}/{}", user.username, name)
    } else {
        format!("{}/{}/{}", user.username, parent, name)
    };
    let existed = match sb.try_exists(&target_rel) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                "[DAV] PUT 目标存在性检查异常，按不存在处理: path={} err={}",
                target_rel,
                e
            );
            false
        }
    };

    // 配额预留必须先于 rename（M3/M4 修复 + 覆盖保护）：
    // 事务内预检+增量（写锁串行化消除 TOCTOU）。delta = 新占用 - 旧占用（覆盖时旧大小释放）。
    // 注意顺序：若预留放在 rename 之后，超配拒绝时旧文件已被原子替换、删除新文件即销毁旧内容
    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            tracing::error!("[DAV] PUT 开启配额事务失败: err={}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let old_contrib: i64 = if existed {
        sqlx::query_scalar(
            "SELECT CEIL(size_mb) FROM files WHERE username = ? AND name = ? AND parent_path = ?",
        )
        .bind(&user.username)
        .bind(&name)
        .bind(&parent)
        .fetch_optional(&mut *tx)
        .await
        .unwrap_or(None)
        .unwrap_or(0.0) as i64
    } else {
        0
    };
    let need_mb = crate::handlers::utils::bytes_to_mb_ceil(written);
    if let Err(e) = crate::handlers::utils::check_and_adjust_quota_tx(
        &mut tx,
        &user.username,
        need_mb - old_contrib,
    )
    .await
    {
        drop(tx);
        let _ = tokio::fs::remove_file(&tmp).await;
        return if matches!(e, crate::error::AppError::Forbidden(_)) {
            StatusCode::INSUFFICIENT_STORAGE
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        }
        .into_response();
    }
    if let Err(e) = tx.commit().await {
        let _ = tokio::fs::remove_file(&tmp).await;
        tracing::error!("[DAV] PUT 配额事务提交失败: err={}", e);
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    if let Err(e) = sb.rename(&tmp_rel, &target_rel) {
        let _ = tokio::fs::remove_file(&tmp).await;
        tracing::error!(
            "[DAV] PUT 落盘 rename 失败: src={} dst={} err={}",
            tmp_rel,
            target_rel,
            e
        );
        // 预留已提交但落盘失败：全量重算自愈（事务化版本）
        let _ = update_user_used_mb(pool, &user.username).await;
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    // 登记：覆盖走 UPDATE（保留行元数据），新建走 INSERT。
    // 失败只影响 DB 行与配额的对账（下轮 update_user_used_mb 自愈），不再触碰文件
    let size_mb = (written as f64 / 1024.0 / 1024.0).ceil();
    let registered = if existed {
        sqlx::query(
            "UPDATE files SET size_mb = ?, identifier = NULL WHERE username = ? AND name = ? AND parent_path = ?",
        )
        .bind(size_mb)
        .bind(&user.username)
        .bind(&name)
        .bind(&parent)
        .execute(pool)
        .await
        .is_ok()
    } else {
        sqlx::query(
            "INSERT INTO files (username, name, parent_path, is_dir, size_mb) VALUES (?, ?, ?, 0, ?)",
        )
        .bind(&user.username)
        .bind(&name)
        .bind(&parent)
        .bind(size_mb)
        .execute(pool)
        .await
        .is_ok()
    };
    if !registered {
        tracing::error!("[DAV] 上传登记失败，触发配额重算自愈: {}/{}", parent, name);
        let _ = update_user_used_mb(pool, &user.username).await;
    }
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

pub(crate) async fn mkcol(pool: &SqlitePool, user: &DavUser, path: &str) -> Response {
    let rel = path.trim_start_matches('/');
    let (parent, name) = split_last(rel);
    if validate_name(&name).is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let full_rel = format!("{}/{}", user.username, rel);
    let parent_rel = format!("{}/{}", user.username, parent);
    let sb = match user_sandbox() {
        Ok(s) => s,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    match sb.try_exists(&full_rel) {
        Ok(true) => return StatusCode::METHOD_NOT_ALLOWED.into_response(),
        Ok(false) => {}
        Err(e) => {
            tracing::warn!(
                "[DAV] MKCOL 目标存在性检查异常: path={} err={}",
                full_rel,
                e
            );
        }
    }
    // 父目录必须已存在（RFC 4918：否则 409；沙箱内判定，越界视为不存在）
    match sb.try_exists(&parent_rel) {
        Ok(true) => {}
        Ok(false) => return StatusCode::CONFLICT.into_response(),
        Err(e) => {
            tracing::warn!(
                "[DAV] MKCOL 父目录存在性检查异常: path={} err={}",
                parent_rel,
                e
            );
            return StatusCode::CONFLICT.into_response();
        }
    }
    if let Err(e) = sb.create_dir(&full_rel) {
        tracing::error!("[DAV] MKCOL 创建目录失败: path={} err={}", full_rel, e);
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

pub(crate) async fn move_or_copy(
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
    // P1-7：d_parent 是相对用户根的目的父路径——会写入 .dav_disp 元数据，崩溃恢复期
    // 按 {username}/{parent}/{fname} 还原。含 `..`/空段/绝对路径时，恶意 Destination
    // 可经恢复流程把文件写进其他用户子树，必须逐段白名单校验（空串=用户根，规范化语义不变）
    let d_parent = match validate_rel_path(&d_parent) {
        Ok(p) => p,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    // 同位置移动 = 无操作
    if src_rel == dest_rel {
        return StatusCode::NO_CONTENT.into_response();
    }

    // 覆盖语义
    let overwrite = !headers
        .get("Overwrite")
        .map(|v| v.as_bytes() == b"F")
        .unwrap_or(false);
    let _dst_full = match physical_path(&user.username, &dest_rel) {
        Ok(p) => p,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let dst_rel_full = format!("{}/{}", user.username, dest_rel);
    let sb = match user_sandbox() {
        Ok(s) => s,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    // P2-17：目标存在性检查——Err 是真实异常，记警告后按“不存在”降级（对外行为不变）
    let dst_exists = match sb.try_exists(&dst_rel_full) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                "[DAV] MOVE/COPY 目标存在性检查异常: path={} err={}",
                dst_rel_full,
                e
            );
            false
        }
    };
    if dst_exists && !overwrite {
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
    // (位移文件 rel, 元数据文件, name, parent, logical, 暂存的 DB 行)
    let mut displaced: Option<(
        String,
        std::path::PathBuf,
        String,
        String,
        String,
        Vec<DisplacedFileRow>,
    )> = None;
    if dst_exists {
        // M9：位移目标放在 uploads/.dav_disp（不在 TMP_DIR 内——24h 临时清扫不得触碰），
        // 并落一份 JSON 元数据（目标路径 + 暂存 DB 行），崩溃后由启动任务 recover_dav_disp 还原
        let disp_uuid = uuid::Uuid::new_v4().to_string();
        let disp_dir = std::path::Path::new(crate::constants::DAV_DISP_DIR);
        if let Err(e) = std::fs::create_dir_all(disp_dir) {
            tracing::error!(
                "[DAV] 创建位移目录失败: path={} err={}",
                disp_dir.display(),
                e
            );
        }
        let disp_rel = format!("{}/{}.d", crate::constants::DAV_DISP_DIR, disp_uuid);
        let _disp_path = disp_dir.join(format!("{disp_uuid}.d"));
        let meta_path = disp_dir.join(format!("{disp_uuid}.json"));
        if let Err(e) = sb.rename(&dst_rel_full, &disp_rel) {
            tracing::error!(
                "[DAV] 覆盖目标位移 rename 失败: src={} dst={} err={}",
                dst_rel_full,
                disp_rel,
                e
            );
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
        let rows: Vec<DisplacedFileRow> = match sqlx::query_as(
            "SELECT name, parent_path, is_dir, size_mb, identifier FROM files WHERE username = ? AND ((name = ? AND parent_path = ?) OR parent_path LIKE ? ESCAPE '\\')",
        )
        .bind(&user.username)
        .bind(&d_name)
        .bind(&d_parent)
        .bind(&child_prefix)
        .fetch_all(pool)
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(
                    "[DAV] 覆盖目标 DB 行暂存查询失败（失败回滚将无法重插旧行）: dest={} err={}",
                    dest_rel,
                    e
                );
                Vec::new()
            }
        };
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
        if let Err(e) = std::fs::write(&meta_path, meta.to_string()) {
            tracing::error!(
                "[DAV] 覆盖位移元数据写入失败（崩溃恢复将无法还原目标）: path={} err={}",
                meta_path.display(),
                e
            );
        }
        let displaced_logical = logical_path(&d_parent, &d_name);
        displaced = Some((
            disp_rel,
            meta_path,
            d_name.clone(),
            d_parent.clone(),
            displaced_logical,
            rows,
        ));
    }

    let (s_parent, s_name) = split_last(src_rel);
    // P1-7：src 侧同样逐段白名单——含 `..` 的请求路径会让 MOVE/COPY 的磁盘意图落到
    // 其他用户子树。rename_core/move_core 内部已有同类校验，但 copy_recursive 没有，
    // 统一在入口拦截可覆盖全部路径（父为空=用户根）
    let s_parent = match validate_rel_path(&s_parent) {
        Ok(p) => p,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    if validate_name(&s_name).is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let result = if is_move {
        // 目标父目录需存在（物理经沙箱逐级创建 + 目录行）
        let dst_parent_rel = format!("{}/{}", user.username, d_parent);
        let _ = sb.create_dir_all(&dst_parent_rel);
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
            if let Some((tmp_rel, meta_path, name, parent, logical, _rows)) = displaced {
                // 成功：DB 行已删，物理位移文件 + 元数据清掉（沙箱内递归删除）
                let _ = sb.remove_dir_all(&tmp_rel);
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
            if let Some((tmp_rel, meta_path, _, _, _, rows)) = displaced {
                let _ = sb.rename(&tmp_rel, &dst_rel_full);
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
pub(crate) async fn copy_recursive(
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

    // L3 修复：记账口径与 upload 路径一致（逐文件 CEIL 后求和）——
    // 历史为 sum().ceil()，多文件 COPY 会少扣约 1MB/文件
    let total_mb: f64 = rows
        .iter()
        .filter(|r| r.2 == 0)
        .map(|r| r.3.ceil())
        .sum::<f64>();

    // 阶段一：磁盘复制（无任何 DB 写入）。沙箱内复制是阻塞 IO，必须移出 async worker
    // 且移出写事务——历史实现把整个拷贝过程包在 SQLite 写事务里：大目录 COPY 会
    // 占死一个 tokio worker + 持写锁数分钟，全站写操作 busy_timeout 后 500。
    let sb = crate::fsutil::Sandbox::new(UPLOADS_DIR)
        .map_err(|e| format!("COPY 沙箱初始化失败: {e}"))?;
    let mut disk_plan: Vec<(String, String, bool, String, String, f64)> =
        Vec::with_capacity(rows.len());
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
        let src_rel = format!("{username}/{parent_path}/{name}");
        let dst_rel = format!("{username}/{new_parent}/{target_name}");
        let dst_parent_rel = format!("{username}/{new_parent}");
        sb.create_dir_all(&dst_parent_rel)
            .map_err(|e| format!("COPY 创建目录失败: {e}"))?;
        disk_plan.push((
            src_rel,
            dst_rel,
            *is_dir != 0,
            new_parent,
            target_name.to_string(),
            *size_mb,
        ));
    }
    for (src_rel, dst_rel, is_dir, _new_parent, _target_name, _size_mb) in &disk_plan {
        if *is_dir {
            sb.create_dir_all(dst_rel)
                .map_err(|e| format!("COPY 创建目录失败: {e}"))?;
        } else {
            let src_rel = src_rel.clone();
            let dst_rel = dst_rel.clone();
            let sb2 = sb.clone();
            tokio::task::spawn_blocking(move || sb2.copy(&src_rel, &dst_rel))
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
        // 仅当目标根路径能安全解析时才清理（openat2 沙箱失败绝不触碰任何目录）
        let root_rel = format!("{username}/{d_parent}/{d_name}");
        let _ = sb.remove_dir_all(&root_rel);
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

pub(crate) async fn delete_item(pool: &SqlitePool, user: &DavUser, path: &str) -> Response {
    let rel = path.trim_start_matches('/');
    if rel.is_empty() {
        return StatusCode::FORBIDDEN.into_response(); // 不允许删除根
    }
    let full_rel = format!("{}/{}", user.username, rel);
    let _full = match physical_path(&user.username, rel) {
        Ok(p) => p,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let sb = match user_sandbox() {
        Ok(s) => s,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    match sb.try_exists(&full_rel) {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::warn!("[DAV] DELETE 存在性检查异常: path={} err={}", full_rel, e);
            return StatusCode::NOT_FOUND.into_response();
        }
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

pub(crate) fn lock_response() -> Response {
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
