use axum::{
    body::Body,
    extract::{Extension, Path, Query},
    http::{HeaderMap, HeaderValue, Request, StatusCode, header},
    response::{IntoResponse, Json},
};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;
use uuid::Uuid;
use zip::write::SimpleFileOptions;

use crate::handlers::BatchDownloadRequest;
use crate::handlers::utils::{log_audit, safe_join_sandbox, update_user_used_mb, user_dir_path};
use pinas_core::UserSession;

#[derive(Deserialize)]
pub struct EditGetQuery {
    pub path: String,
}

#[derive(Deserialize)]
pub struct EditSaveRequest {
    pub path: String,
    pub content: String,
}

/// 编辑器读取的最大文件大小 (50 MB，防止内存耗尽)
use crate::constants::MAX_EDITOR_READ_SIZE_BYTES as MAX_EDITOR_READ_SIZE;

// 编辑器读取（只读，不记录审计日志）
pub async fn get_file_content_handler(
    Extension(session): Extension<UserSession>,
    Query(query): Query<EditGetQuery>,
) -> impl IntoResponse {
    let username = &session.username;
    let base_path = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let full_p = safe_join_sandbox(base_path, &format!("{}/{}", username, query.path));

    // 检查文件大小，防止读取超大文件耗尽内存
    if let Ok(meta) = tokio::fs::metadata(&full_p).await
        && meta.len() > MAX_EDITOR_READ_SIZE
    {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "文件过大（{} MB），编辑器最大支持 {} MB",
                meta.len() / 1024 / 1024,
                MAX_EDITOR_READ_SIZE / 1024 / 1024
            ),
        )
            .into_response();
    }

    match tokio::fs::read_to_string(&full_p).await {
        Ok(text) => (StatusCode::OK, text).into_response(),
        Err(e) => {
            tracing::error!("[Media] 读取文件失败: {}", e);
            (StatusCode::NOT_FOUND, "文件不可读或非文本类型").into_response()
        }
    }
}

/// 编辑器保存的最大文件大小 (10 MB)
use crate::constants::MAX_EDIT_SAVE_SIZE_BYTES as MAX_EDIT_SIZE;

// 编辑器保存
pub async fn save_file_content_handler(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<EditSaveRequest>,
) -> impl IntoResponse {
    if payload.content.len() > MAX_EDIT_SIZE {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "文件过大，编辑器最大支持 {} MB",
                MAX_EDIT_SIZE / 1024 / 1024
            ),
        )
            .into_response();
    }

    let username = &session.username;
    let base_path = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let full_p = safe_join_sandbox(base_path, &format!("{}/{}", username, payload.path));

    if let Err(e) = tokio::fs::write(&full_p, payload.content.as_bytes()).await {
        tracing::error!("[Media] 写入文件失败: {}", e);
        return (StatusCode::INTERNAL_SERVER_ERROR, "写入文件失败，请重试").into_response();
    }

    let p = std::path::Path::new(&payload.path);
    let name = p
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let parent = p
        .parent()
        .unwrap_or(std::path::Path::new(""))
        .to_string_lossy()
        .to_string();
    let parent_cleaned = if parent == "/" {
        "".to_string()
    } else {
        parent
    };

    let meta = full_p.metadata().map(|m| m.len()).unwrap_or(0);
    let size_mb_exact = meta as f64 / crate::handlers::utils::BYTES_PER_MB_F64;

    let _ = sqlx::query(
        "UPDATE files SET size_mb = ? WHERE username = ? AND name = ? AND parent_path = ?",
    )
    .bind(size_mb_exact)
    .bind(username)
    .bind(&name)
    .bind(&parent_cleaned)
    .execute(&pool)
    .await;

    let _ = update_user_used_mb(&pool, username).await;

    // 审计日志：保存文件
    let _ = log_audit(
        &pool,
        username,
        "edit_save",
        Some(&payload.path),
        Some(&format!("{:.2} MB", size_mb_exact)),
        None,
        None,
    )
    .await;

    (StatusCode::OK, "在线保存成功").into_response()
}

// 多媒体代理（支持流式播放、拖拽进度条、Range 请求）
// 使用 HEAD 方法返回元数据（Content-Type + Accept-Ranges），GET 返回文件内容
#[tracing::instrument(skip(session, req))]
pub async fn media_proxy(
    Extension(session): Extension<UserSession>,
    Path(raw_path): Path<String>,
    req: Request<Body>,
) -> impl IntoResponse {
    let username = &session.username;
    let base_path = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let full_path = safe_join_sandbox(base_path, &format!("{}/{}", username, raw_path));

    if !full_path.exists() {
        return StatusCode::NOT_FOUND.into_response();
    }

    let metadata = match tokio::fs::metadata(&full_path).await {
        Ok(meta) => meta,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let file_size = metadata.len();
    let mime = normalize_media_mime(
        &full_path,
        mime_guess::from_path(&full_path).first_or_octet_stream(),
    );

    // HEAD 请求：仅返回元数据头（浏览器用于探测 Range 支持）
    if req.method() == axum::http::Method::HEAD {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            mime.to_string()
                .parse()
                .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
        );
        headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(file_size));
        return (StatusCode::OK, headers).into_response();
    }

    let range_header = req
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok());
    let (start, end, status) = if let Some(range_value) = range_header {
        if let Some((start, end)) = parse_range(range_value, file_size) {
            (start, end, StatusCode::PARTIAL_CONTENT)
        } else {
            return (StatusCode::RANGE_NOT_SATISFIABLE, HeaderMap::new()).into_response();
        }
    } else {
        // 无 Range 头时仅返回前 2 MB，避免一次性流式传输整个大文件
        // 浏览器获取元数据后会自动发送 Range 请求获取更多数据
        (
            0,
            (file_size - 1).min(2 * 1024 * 1024 - 1),
            StatusCode::PARTIAL_CONTENT,
        )
    };

    let length = end - start + 1;

    let mut file = match tokio::fs::File::open(&full_path).await {
        Ok(f) => f,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let limited_file = file.take(length);
    let stream = ReaderStream::new(limited_file);
    let body = Body::from_stream(stream);

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        mime.to_string()
            .parse()
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    // 禁用 gzip 压缩 — 视频/音频已是压缩格式，再压缩会缓冲整个响应导致无法 seek
    headers.insert(
        header::CONTENT_ENCODING,
        HeaderValue::from_static("identity"),
    );

    if status == StatusCode::PARTIAL_CONTENT {
        let content_range = format!("bytes {}-{}/{}", start, end, file_size);
        headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&content_range)
                .unwrap_or_else(|_| HeaderValue::from_static("bytes */0")),
        );
    }

    (status, headers, body).into_response()
}

/// 将非浏览器的 MIME 类型规范化映射为浏览器兼容的类型
fn normalize_media_mime(
    path: &std::path::Path,
    default_mime: mime_guess::Mime,
) -> mime_guess::Mime {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        // M4V 属于 MPEG-4 容器，映射为 video/mp4 以确保浏览器兼容性
        "m4v" | "mp4v" => "video/mp4".parse().unwrap_or(default_mime),
        // M4A 是 AAC 音频在 MP4 容器中
        "m4a" => "audio/mp4".parse().unwrap_or(default_mime),
        _ => default_mime,
    }
}

/// 解析 Range 头，返回 (start, end)。仅支持单个范围 bytes=start-end。
/// 如果 range 无效或超出文件范围，返回 None。
fn parse_range(range_value: &str, file_size: u64) -> Option<(u64, u64)> {
    if !range_value.starts_with("bytes=") {
        return None;
    }
    let range_spec = &range_value[6..];
    let mut parts = range_spec.splitn(2, '-');
    let start_str = parts.next().unwrap_or("");
    let end_str = parts.next().unwrap_or("");

    if start_str.is_empty() && end_str.is_empty() {
        return None;
    }

    // 后缀范围：bytes=-suffix
    if start_str.is_empty() {
        let suffix: u64 = end_str.parse().ok()?;
        if suffix == 0 || suffix > file_size {
            return None;
        }
        let start = file_size - suffix;
        let end = file_size - 1;
        return Some((start, end));
    }

    // 正常范围：bytes=start-end 或 bytes=start-
    let start: u64 = start_str.parse().ok()?;
    if start >= file_size {
        return None;
    }

    let end = if end_str.is_empty() {
        file_size - 1
    } else {
        let e: u64 = end_str.parse().ok()?;
        if e < start {
            return None;
        }
        e.min(file_size - 1)
    };

    Some((start, end))
}

// 批量下载 ZIP（使用临时文件，流式发送）
pub async fn download_zip(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<BatchDownloadRequest>,
) -> impl IntoResponse {
    let username = session.username;
    let parent_path = user_dir_path(payload.current_path);
    let base_path = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let user_root = base_path.join(&username);
    let base_dir = if parent_path.is_empty() {
        user_root.clone()
    } else {
        user_root.join(&parent_path)
    };

    let mut items = Vec::new();
    for name in &payload.names {
        // 使用 safe_join_sandbox 而非 Path::starts_with，后者不规范化 .. 组件，
        // 可能导致路径穿越攻击（如 ../../../etc/passwd）
        let target_path = safe_join_sandbox(&base_dir, name);
        if target_path.starts_with(&user_root) && target_path.exists() {
            items.push((name.clone(), target_path));
        }
    }

    if items.is_empty() {
        return (StatusCode::BAD_REQUEST, "没有有效的文件或目录可打包").into_response();
    }

    // 审计日志：记录打包下载（异步执行，不阻塞响应）
    let item_names: Vec<String> = items.iter().map(|(name, _)| name.clone()).collect();
    let details = if item_names.len() <= 5 {
        item_names.join(", ")
    } else {
        format!("{} 个项目", item_names.len())
    };
    let target = if parent_path.is_empty() {
        "根目录".to_string()
    } else {
        parent_path.clone()
    };
    let _ = log_audit(
        &pool,
        &username,
        "download_zip",
        Some(&target),
        Some(&details),
        None,
        None,
    )
    .await;

    let temp_dir = std::env::temp_dir();
    let temp_file_name = format!("zip_{}.tmp", Uuid::new_v4());
    let temp_path = temp_dir.join(&temp_file_name);
    let temp_path_clone = temp_path.clone();

    let zip_result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let file = std::fs::File::create(&temp_path).map_err(|e| e.to_string())?;
        let mut zip_writer = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::DEFLATE)
            .unix_permissions(0o644);

        for (name, full_path) in items {
            if full_path.is_file() {
                let mut file = std::fs::File::open(&full_path).map_err(|e| e.to_string())?;
                zip_writer
                    .start_file(&name, options)
                    .map_err(|e| e.to_string())?;
                std::io::copy(&mut file, &mut zip_writer).map_err(|e| e.to_string())?;
            } else if full_path.is_dir() {
                zip_writer
                    .add_directory(&name, SimpleFileOptions::default())
                    .map_err(|e| e.to_string())?;
                add_dir_to_zip_sync(&mut zip_writer, &full_path, &name, options)?;
            }
        }
        zip_writer.finish().map_err(|e| e.to_string())?;
        Ok(())
    })
    .await;

    let zip_inner = match zip_result {
        Ok(inner) => inner,
        Err(join_err) => {
            let _ = tokio::fs::remove_file(&temp_path_clone).await;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("压缩任务失败: {}", join_err),
            )
                .into_response();
        }
    };
    if let Err(e) = zip_inner {
        let _ = tokio::fs::remove_file(&temp_path_clone).await;
        tracing::error!("[Media] 压缩失败: {}", e);
        return (StatusCode::INTERNAL_SERVER_ERROR, "文件压缩失败，请重试").into_response();
    }

    match tokio::fs::File::open(&temp_path_clone).await {
        Ok(file) => {
            let path_for_cleanup = temp_path_clone.clone();
            // 延迟清理临时文件（10 分钟，足够大多数下载完成）
            tokio::spawn(async move {
                tokio::time::sleep(tokio::time::Duration::from_secs(600)).await;
                let _ = tokio::fs::remove_file(&path_for_cleanup).await;
            });
            let stream = ReaderStream::new(file);
            let body = Body::from_stream(stream);
            let headers = [
                ("content-type", "application/zip"),
                (
                    "content-disposition",
                    "attachment; filename=\"cloud_download.zip\"",
                ),
            ];
            (StatusCode::OK, headers, body).into_response()
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&temp_path_clone).await;
            {
                tracing::error!("[Media] 临时文件失败: {}", e);
                (StatusCode::INTERNAL_SERVER_ERROR, "文件操作失败，请重试").into_response()
            }
        }
    }
}

// 辅助：同步递归添加目录到 ZIP
fn add_dir_to_zip_sync(
    zip_writer: &mut zip::ZipWriter<std::fs::File>,
    dir_path: &std::path::Path,
    zip_prefix: &str,
    options: SimpleFileOptions,
) -> Result<(), String> {
    for entry in std::fs::read_dir(dir_path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let zip_name = format!("{}/{}", zip_prefix, name);
        if path.is_file() {
            let mut file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
            zip_writer
                .start_file(&zip_name, options)
                .map_err(|e| e.to_string())?;
            std::io::copy(&mut file, zip_writer).map_err(|e| e.to_string())?;
        } else if path.is_dir() {
            zip_writer
                .add_directory(&zip_name, SimpleFileOptions::default())
                .map_err(|e| e.to_string())?;
            add_dir_to_zip_sync(zip_writer, &path, &zip_name, options)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_range() {
        let file_size = 1000;
        assert_eq!(parse_range("bytes=0-99", file_size), Some((0, 99)));
        assert_eq!(parse_range("bytes=100-", file_size), Some((100, 999)));
        assert_eq!(parse_range("bytes=-50", file_size), Some((950, 999)));
        assert_eq!(parse_range("bytes=500-600", file_size), Some((500, 600)));
        assert_eq!(parse_range("bytes=1000-2000", file_size), None);
        assert_eq!(parse_range("bytes=200-100", file_size), None);
        assert_eq!(parse_range("bytes=", file_size), None);
        assert_eq!(parse_range("invalid", file_size), None);
    }
}
