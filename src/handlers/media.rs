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

use crate::core::UserSession;
use crate::handlers::BatchDownloadRequest;
use crate::handlers::utils::{log_audit, safe_join_sandbox, user_dir_path};

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
    let full_p = match safe_join_sandbox(base_path, &format!("{}/{}", username, query.path)) {
        Ok(p) => p,
        Err(_) => return (StatusCode::NOT_FOUND, "文件不存在").into_response(),
    };

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
    let full_p = match safe_join_sandbox(base_path, &format!("{}/{}", username, payload.path)) {
        Ok(p) => p,
        Err(_) => return (StatusCode::BAD_REQUEST, "非法路径").into_response(),
    };

    // 配额检查：按新旧大小差值计（改小反而释放空间；改大计入差额）
    let old_len = tokio::fs::metadata(&full_p)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    let new_len = payload.content.len() as u64;
    if new_len > old_len {
        let delta_mb = crate::handlers::utils::bytes_to_mb_ceil(new_len - old_len);
        let row: Option<(i64, i64)> =
            match sqlx::query_as("SELECT used_mb, quota_mb FROM users WHERE username = ?")
                .bind(username)
                .fetch_optional(&pool)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("[Media] 查询用户配额失败: {}", e);
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "服务器内部错误，请稍后重试",
                    )
                        .into_response();
                }
            };
        let Some((current_used, quota)) = row else {
            return (StatusCode::NOT_FOUND, "用户不存在").into_response();
        };
        if current_used + delta_mb > quota {
            return (StatusCode::FORBIDDEN, "存储空间不足，超出配额").into_response();
        }
    }

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

    // 增量调整配额（新旧字节差），替代全表 SUM 重算
    let old_len_mb = crate::handlers::utils::bytes_to_mb_ceil(old_len);
    let new_len_mb = crate::handlers::utils::bytes_to_mb_ceil(new_len);
    crate::handlers::utils::adjust_user_used_mb(&pool, username, new_len_mb - old_len_mb).await;

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
    let full_path = match safe_join_sandbox(base_path, &format!("{}/{}", username, raw_path)) {
        Ok(p) => p,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };

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
    let mime_str = mime.to_string();
    // html/svg/xml 等内联渲染可执行脚本 → 强制 octet-stream（存储型 XSS 通道封堵）
    let force_download = crate::handlers::utils::is_force_download_mime(&mime_str);
    let content_type = if force_download {
        HeaderValue::from_static("application/octet-stream")
    } else {
        mime_str
            .parse()
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"))
    };

    // 空文件：直接返回 200 空体（避免 (file_size-1) 下溢为 u64::MAX）
    if file_size == 0 {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, content_type);
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(0u64));
        return (StatusCode::OK, headers, Body::empty()).into_response();
    }

    // HEAD 请求：仅返回元数据头（浏览器用于探测 Range 支持）
    if req.method() == axum::http::Method::HEAD {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, content_type);
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
            // RFC 7233 §4.4：416 必须携带 Content-Range: bytes */{size}
            let mut h416 = HeaderMap::new();
            h416.insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{}", file_size))
                    .unwrap_or_else(|_| HeaderValue::from_static("bytes */0")),
            );
            return (StatusCode::RANGE_NOT_SATISFIABLE, h416).into_response();
        }
    } else {
        // 无 Range 头时返回 200 全量（标准服务器行为）。
        // 修复:此前只返前 2MB 导致音频播放中断(部分播放器只发一次请求)与
        // moov 不在文件头部的视频无法解析(浏览器 preload=metadata 探测拿不到 moov)。
        (0, file_size - 1, StatusCode::OK)
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
    headers.insert(header::CONTENT_TYPE, content_type);
    if force_download && let Ok(d) = "attachment".parse::<HeaderValue>() {
        headers.insert(header::CONTENT_DISPOSITION, d);
    }
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    // 注意：不得设置 Content-Encoding(含 identity)——Chromium 媒体管线对带该头的 206
    // 响应判定为"需解码"直接拒绝(SRC_NOT_SUPPORTED)。防 gzip 压缩改由 CompressionLayer
    // 谓词跳过 /api/media/ 路径(见 router.rs)。

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
pub(crate) fn parse_range(range_value: &str, file_size: u64) -> Option<(u64, u64)> {
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
        // 可能导致路径穿越攻击（如 ../../../etc/passwd）；非法路径直接跳过
        let Ok(target_path) = safe_join_sandbox(&base_dir, name) else {
            continue;
        };
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
        // M2：打包预算——总字节 + 条目数双重上限，防反向 zip bomb（超大目录/符号链接环
        // 打爆 tmpfs 或 SD 卡）；符号链接一律跳过（不跟随，杜绝递归环与越界打包）
        let mut budget = ZipBudget::new();

        for (name, full_path) in items {
            // 条目名净化：跳过 "."/".."（防 zip-slip 解压穿越）
            if name == "." || name == ".." {
                continue;
            }
            let meta = std::fs::symlink_metadata(&full_path).map_err(|e| e.to_string())?;
            if meta.file_type().is_symlink() {
                tracing::warn!("[Media] zip 跳过符号链接: {:?}", full_path);
                continue;
            }
            if meta.is_file() {
                budget.charge(meta.len())?;
                let mut file = std::fs::File::open(&full_path).map_err(|e| e.to_string())?;
                zip_writer
                    .start_file(&name, options)
                    .map_err(|e| e.to_string())?;
                std::io::copy(&mut file, &mut zip_writer).map_err(|e| e.to_string())?;
            } else if meta.is_dir() {
                budget.charge(0)?;
                zip_writer
                    .add_directory(&name, SimpleFileOptions::default())
                    .map_err(|e| e.to_string())?;
                add_dir_to_zip_sync(&mut zip_writer, &full_path, &name, options, &mut budget)?;
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
            tracing::error!("[Media] 压缩任务失败: {}", join_err);
            return (StatusCode::INTERNAL_SERVER_ERROR, "文件压缩失败，请重试").into_response();
        }
    };
    if let Err(e) = zip_inner {
        let _ = tokio::fs::remove_file(&temp_path_clone).await;
        tracing::error!("[Media] 压缩失败: {}", e);
        return (StatusCode::INTERNAL_SERVER_ERROR, "文件压缩失败，请重试").into_response();
    }

    match tokio::fs::File::open(&temp_path_clone).await {
        Ok(file) => {
            // 临时文件在流结束时删除（替代固定 600s 延迟删除：
            // 慢链路下大 zip 可能未传完就被删，且崩溃时残留堆积）
            let body = Body::from_stream(ReaderStream::new(DeleteOnDropFile {
                inner: file,
                path: temp_path_clone,
            }));
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

/// 流式传输文件，Drop 时删除临时文件（连接完成或中断均触发）
struct DeleteOnDropFile {
    inner: tokio::fs::File,
    path: std::path::PathBuf,
}

impl tokio::io::AsyncRead for DeleteOnDropFile {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl Drop for DeleteOnDropFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// M2：打包预算上限（总字节 + 条目数），防反向 zip bomb 打爆 tmpfs/SD 卡
const ZIP_MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const ZIP_MAX_ENTRIES: usize = 10_000;

struct ZipBudget {
    total_bytes: u64,
    entries: usize,
}

impl ZipBudget {
    fn new() -> Self {
        Self {
            total_bytes: 0,
            entries: 0,
        }
    }

    fn charge(&mut self, bytes: u64) -> Result<(), String> {
        if self.entries + 1 > ZIP_MAX_ENTRIES {
            return Err(format!("打包条目数超过上限 {ZIP_MAX_ENTRIES}"));
        }
        if self.total_bytes.saturating_add(bytes) > ZIP_MAX_TOTAL_BYTES {
            return Err(format!(
                "打包总大小超过上限 {} GB",
                ZIP_MAX_TOTAL_BYTES / 1024 / 1024 / 1024
            ));
        }
        self.entries += 1;
        self.total_bytes += bytes;
        Ok(())
    }
}

// 辅助：同步递归添加目录到 ZIP（符号链接跳过，预算逐条扣减）
fn add_dir_to_zip_sync(
    zip_writer: &mut zip::ZipWriter<std::fs::File>,
    dir_path: &std::path::Path,
    zip_prefix: &str,
    options: SimpleFileOptions,
    budget: &mut ZipBudget,
) -> Result<(), String> {
    for entry in std::fs::read_dir(dir_path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        // 跳过 "."/".." 条目（zip-slip 防御）
        if name == "." || name == ".." {
            continue;
        }
        // dirent 的 file_type 不跟随符号链接：符号链接一律跳过（防递归环/越界打包）
        let ftype = entry.file_type().map_err(|e| e.to_string())?;
        if ftype.is_symlink() {
            tracing::warn!("[Media] zip 跳过符号链接: {:?}", path);
            continue;
        }
        let zip_name = format!("{}/{}", zip_prefix, name);
        if ftype.is_file() {
            let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
            budget.charge(meta.len())?;
            let mut file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
            zip_writer
                .start_file(&zip_name, options)
                .map_err(|e| e.to_string())?;
            std::io::copy(&mut file, zip_writer).map_err(|e| e.to_string())?;
        } else if ftype.is_dir() {
            budget.charge(0)?;
            zip_writer
                .add_directory(&zip_name, SimpleFileOptions::default())
                .map_err(|e| e.to_string())?;
            add_dir_to_zip_sync(zip_writer, &path, &zip_name, options, budget)?;
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
