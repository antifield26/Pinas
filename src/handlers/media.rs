use axum::{
    body::Body,
    extract::{Extension, Path, Query},
    http::{header, HeaderMap, HeaderValue, StatusCode, Request},
    response::{IntoResponse, Json},
};
use serde::Deserialize;
use tokio_util::io::ReaderStream;
use uuid::Uuid;
use zip::write::FileOptions;
use tokio::io::{AsyncSeekExt, AsyncReadExt};

use crate::handlers::utils::{safe_join_sandbox, user_dir_path, update_user_used_mb, log_audit, bytes_to_mb_string};
use crate::handlers::BatchDownloadRequest;
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
    if let Ok(meta) = tokio::fs::metadata(&full_p).await {
        if meta.len() > MAX_EDITOR_READ_SIZE {
            return (StatusCode::BAD_REQUEST,
                format!("文件过大（{} MB），编辑器最大支持 {} MB",
                    meta.len() / 1024 / 1024, MAX_EDITOR_READ_SIZE / 1024 / 1024)
            ).into_response();
        }
    }

    match tokio::fs::read_to_string(&full_p).await {
        Ok(text) => (StatusCode::OK, text).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, format!("不可读或并非文本类型: {}", e)).into_response(),
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
        return (StatusCode::BAD_REQUEST, format!("文件过大，编辑器最大支持 {} MB", MAX_EDIT_SIZE / 1024 / 1024)).into_response();
    }

    let username = &session.username;
    let base_path = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let full_p = safe_join_sandbox(base_path, &format!("{}/{}", username, payload.path));

    if let Err(e) = tokio::fs::write(&full_p, payload.content.as_bytes()).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("写入文件失败: {}", e)).into_response();
    }

    let p = std::path::Path::new(&payload.path);
    let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
    let parent = p.parent().unwrap_or(std::path::Path::new("")).to_string_lossy().to_string();
    let parent_cleaned = if parent == "/" { "".to_string() } else { parent };

    let meta = full_p.metadata().map(|m| m.len()).unwrap_or(0);
    let size_mb = bytes_to_mb_string(meta);

    let _ = sqlx::query("UPDATE files SET size_mb = ? WHERE username = ? AND name = ? AND parent_path = ?")
        .bind(&size_mb).bind(username).bind(&name).bind(&parent_cleaned).execute(&pool).await;

    let _ = update_user_used_mb(&pool, username).await;

    // 审计日志：保存文件
    let _ = log_audit(&pool, username, "edit_save", Some(&payload.path), Some(&size_mb), None, None).await;

    (StatusCode::OK, "在线保存成功").into_response()
}

// 多媒体代理（只读，不记录审计日志）
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

    let range_header = req.headers().get(header::RANGE).and_then(|v| v.to_str().ok());
    let (start, end, status) = if let Some(range_value) = range_header {
        if let Some((start, end)) = parse_range(range_value, file_size) {
            (start, end, StatusCode::PARTIAL_CONTENT)
        } else {
            return (StatusCode::RANGE_NOT_SATISFIABLE).into_response();
        }
    } else {
        (0, file_size - 1, StatusCode::OK)
    };

    let length = end - start + 1;

    let mut file = match tokio::fs::File::open(&full_path).await {
        Ok(f) => f,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if let Err(_) = file.seek(std::io::SeekFrom::Start(start)).await {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let limited_file = file.take(length);
    let stream = ReaderStream::new(limited_file);
    let body = Body::from_stream(stream);

    let mime = mime_guess::from_path(&full_path).first_or_octet_stream();

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, mime.to_string().parse().unwrap());
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));

    if status == StatusCode::PARTIAL_CONTENT {
        let content_range = format!("bytes {}-{}/{}", start, end, file_size);
        headers.insert(header::CONTENT_RANGE, HeaderValue::from_str(&content_range).unwrap());
    }

    (status, headers, body).into_response()
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
    let _ = log_audit(&pool, &username, "download_zip", Some(&target), Some(&details), None, None).await;

    let temp_dir = std::env::temp_dir();
    let temp_file_name = format!("zip_{}.tmp", Uuid::new_v4());
    let temp_path = temp_dir.join(&temp_file_name);
    let temp_path_clone = temp_path.clone();

    let zip_result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let file = std::fs::File::create(&temp_path).map_err(|e| e.to_string())?;
        let mut zip_writer = zip::ZipWriter::new(file);
        let options = FileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);

        for (name, full_path) in items {
            if full_path.is_file() {
                let mut file = std::fs::File::open(&full_path).map_err(|e| e.to_string())?;
                zip_writer.start_file(&name, options).map_err(|e| e.to_string())?;
                std::io::copy(&mut file, &mut zip_writer).map_err(|e| e.to_string())?;
            } else if full_path.is_dir() {
                add_dir_to_zip_sync(&mut zip_writer, &full_path, &name, options)?;
            }
        }
        zip_writer.finish().map_err(|e| e.to_string())?;
        Ok(())
    }).await;

    let zip_inner = match zip_result {
        Ok(inner) => inner,
        Err(join_err) => {
            let _ = tokio::fs::remove_file(&temp_path_clone).await;
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("压缩任务失败: {}", join_err)).into_response();
        }
    };
    if let Err(e) = zip_inner {
        let _ = tokio::fs::remove_file(&temp_path_clone).await;
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("压缩失败: {}", e)).into_response();
    }

    match tokio::fs::File::open(&temp_path_clone).await {
        Ok(file) => {
            let path_for_cleanup = temp_path_clone.clone();
            tokio::spawn(async move {
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                let _ = tokio::fs::remove_file(path_for_cleanup).await;
            });
            let stream = ReaderStream::new(file);
            let body = Body::from_stream(stream);
            let headers = [
                ("content-type", "application/zip"),
                ("content-disposition", "attachment; filename=\"cloud_download.zip\""),
            ];
            (StatusCode::OK, headers, body).into_response()
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&temp_path_clone).await;
            (StatusCode::INTERNAL_SERVER_ERROR, format!("打开临时文件失败: {}", e)).into_response()
        }
    }
}

// 辅助：同步递归添加目录到 ZIP
fn add_dir_to_zip_sync(
    zip_writer: &mut zip::ZipWriter<std::fs::File>,
    dir_path: &std::path::Path,
    zip_prefix: &str,
    options: zip::write::FileOptions,
) -> Result<(), String> {
    for entry in std::fs::read_dir(dir_path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let zip_name = format!("{}/{}", zip_prefix, name);
        if path.is_file() {
            let mut file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
            zip_writer.start_file(&zip_name, options).map_err(|e| e.to_string())?;
            std::io::copy(&mut file, zip_writer).map_err(|e| e.to_string())?;
        } else if path.is_dir() {
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