use axum::{
    extract::{Extension, Multipart, Query},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::error::{AppError, AppResult};
use crate::handlers::utils::{
    bytes_to_mb_ceil, is_allowed_mime, is_allowed_mime_streaming, is_blocked_extension, log_audit,
    safe_join_sandbox, user_dir_path,
};
use pinas_core::UserSession;

// --- DTOs ---
#[derive(Deserialize)]
pub struct CheckQuery {
    pub identifier: String,
}

#[derive(Serialize)]
pub struct CheckResponse {
    pub exists: bool,
    pub chunk_exists: Option<bool>,
    pub uploaded_chunks: Option<Vec<i32>>,
}

#[derive(Deserialize)]
pub struct ChunkParams {
    pub identifier: String,
    pub chunk_index: i32,
    pub total_chunks: i32,
}

#[derive(Deserialize)]
pub struct MergeRequest {
    pub identifier: String,
    pub file_name: String,
    pub parent_path: String,
}

/// 合并过程失败时自动清理临时文件和目录的 RAII guard
struct MergeCleanup {
    target_file: std::path::PathBuf,
    tmp_dir: String,
    armed: bool,
}

impl MergeCleanup {
    fn new(target_file: std::path::PathBuf, tmp_dir: String) -> Self {
        Self {
            target_file,
            tmp_dir,
            armed: true,
        }
    }
    /// 提交成功，取消清理
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for MergeCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.target_file);
            let _ = std::fs::remove_dir_all(&self.tmp_dir);
        }
    }
}

// --- 3. 文件分片秒传/断点续传检查 ---
pub async fn check_chunk(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Query(query): Query<CheckQuery>,
) -> AppResult<Json<CheckResponse>> {
    let username = &session.username;
    let identifier = &query.identifier;
    validate_identifier(identifier)?;

    // 检查文件是否已完全上传（通过文件的 content identifier 匹配）
    let file_exists =
        sqlx::query("SELECT 1 FROM files WHERE username = ? AND identifier = ? LIMIT 1")
            .bind(username)
            .bind(identifier)
            .fetch_optional(&pool)
            .await
            .unwrap_or(None)
            .is_some();

    if file_exists {
        return Ok(Json(CheckResponse {
            exists: true,
            chunk_exists: None,
            uploaded_chunks: None,
        }));
    }

    // 扫描临时分片目录，获取已上传的分片索引
    let tmp_dir = format!("{}/{}", crate::constants::TMP_DIR, identifier);
    let mut uploaded_chunks = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&tmp_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if let Some(idx_str) = entry.file_name().to_str()
                && let Ok(idx) = idx_str.parse::<i32>()
            {
                uploaded_chunks.push(idx);
            }
        }
    }
    uploaded_chunks.sort();

    Ok(Json(CheckResponse {
        exists: false,
        chunk_exists: None,
        uploaded_chunks: Some(uploaded_chunks),
    }))
}

// --- 4. 处理分片上传（流式写入磁盘，避免内存缓冲） ---
use crate::constants::MAX_CHUNK_SIZE_BYTES as MAX_CHUNK_SIZE;

/// 校验上传标识符安全性（仅允许字母数字及连字符，防止路径穿越）
fn validate_identifier(identifier: &str) -> AppResult<()> {
    if identifier.is_empty()
        || !identifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(AppError::bad_request("非法的文件标识符"));
    }
    Ok(())
}

pub async fn upload_chunk(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Query(params): Query<ChunkParams>,
    mut multipart: Multipart,
) -> AppResult<(StatusCode, &'static str)> {
    // 速率限制：每个用户每分钟最多 120 个分片（2 GB/min）
    if !crate::handlers::rate_limit::check_rate_limit(
        &session.username,
        120,
        std::time::Duration::from_secs(60),
    )
    .await
    {
        return Err(AppError::TooManyRequests("上传过于频繁，请稍后再试".into()));
    }

    // 校验分片索引合法性
    if params.chunk_index < 0 || params.chunk_index >= params.total_chunks {
        return Err(AppError::bad_request("分片索引超出范围"));
    }
    if params.total_chunks <= 0 || params.total_chunks > crate::constants::MAX_CHUNKS_PER_FILE {
        return Err(AppError::bad_request("总分片数不合法"));
    }
    validate_identifier(&params.identifier)?;

    // 确保临时目录存在
    let tmp_dir = format!("{}/{}", crate::constants::TMP_DIR, params.identifier);
    let _ = tokio::fs::create_dir_all(&tmp_dir).await;
    let chunk_path = format!("{}/{}", tmp_dir, params.chunk_index);

    // 直接创建文件，流式写入
    let mut file = tokio::fs::File::create(&chunk_path)
        .await
        .map_err(|e| AppError::internal_log("创建分片文件", e))?;

    let mut total_written: u64 = 0;
    let mut mime_buf = [0u8; crate::constants::MIME_HEADER_BUF_SIZE];
    let mut mime_read: usize = 0;
    let is_first_chunk = params.chunk_index == 0;

    while let Ok(Some(mut field)) = multipart.next_field().await {
        if field.name() == Some("file") {
            while let Ok(Some(chunk)) = field.chunk().await {
                total_written += chunk.len() as u64;
                if total_written > MAX_CHUNK_SIZE {
                    let _ = tokio::fs::remove_file(&chunk_path).await;
                    return Err(AppError::payload_too_large("分片超过 100 MB 上限"));
                }

                // 首个分片：保留前 512 字节用于 MIME 检测
                if is_first_chunk && mime_read < crate::constants::MIME_HEADER_BUF_SIZE {
                    let to_copy = std::cmp::min(
                        chunk.len(),
                        crate::constants::MIME_HEADER_BUF_SIZE - mime_read,
                    );
                    mime_buf[mime_read..mime_read + to_copy].copy_from_slice(&chunk[..to_copy]);
                    mime_read += to_copy;
                }

                if let Err(e) = tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await {
                    let _ = tokio::fs::remove_file(&chunk_path).await;
                    return Err(AppError::internal_log("写入分片", e));
                }
            }
        }
    }

    if total_written == 0 {
        let _ = tokio::fs::remove_file(&chunk_path).await;
        return Err(AppError::bad_request("分片数据流为空"));
    }

    // 首分片 MIME 安全校验
    if is_first_chunk && !is_allowed_mime(&mime_buf[..mime_read]) {
        let _ = tokio::fs::remove_file(&chunk_path).await;
        return Err(AppError::forbidden("非法执行文件伪装漏洞防护阻断"));
    }

    // 记录总分数（若不存在则插入）
    let _ = sqlx::query(
        "INSERT OR IGNORE INTO upload_chunks (username, identifier, total_chunks) VALUES (?, ?, ?)",
    )
    .bind(&session.username)
    .bind(&params.identifier)
    .bind(params.total_chunks)
    .execute(&pool)
    .await;

    Ok((StatusCode::OK, "分片暂存成功"))
}

// --- 5. 分片合并（读取所有已上传分片，按索引排序合并）---
#[tracing::instrument(skip(pool, session, payload))]
pub async fn merge_chunks(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<MergeRequest>,
) -> AppResult<(StatusCode, &'static str)> {
    let username = &session.username;
    if is_blocked_extension(&payload.file_name) {
        return Err(AppError::forbidden(
            "安全策略阻断：不允许上传高危执行文件扩展名",
        ));
    }
    validate_identifier(&payload.identifier)?;

    // 从数据库获取总分片数（可选，也可直接从文件系统推断）
    let total_chunks: Option<i32> = sqlx::query_scalar(
        "SELECT total_chunks FROM upload_chunks WHERE username = ? AND identifier = ?",
    )
    .bind(username)
    .bind(&payload.identifier)
    .fetch_optional(&pool)
    .await
    .unwrap_or(None);

    let tmp_dir = format!("{}/{}", crate::constants::TMP_DIR, payload.identifier);
    // 读取目录下所有分片文件
    let mut chunks = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&tmp_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if let Some(idx_str) = entry.file_name().to_str()
                && let Ok(idx) = idx_str.parse::<i32>()
            {
                chunks.push(idx);
            }
        }
    }
    if chunks.is_empty() {
        return Err(AppError::bad_request("未找到任何分片数据"));
    }
    chunks.sort();

    // 如果已知总分片数，校验是否完整
    if let Some(total) = total_chunks
        && chunks.len() != total as usize
    {
        return Err(AppError::bad_request(format!(
            "分片不完整，已上传 {} 个，预期 {} 个",
            chunks.len(),
            total
        )));
    }

    let parent_path = user_dir_path(Some(payload.parent_path));
    let base_path = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let user_dir = safe_join_sandbox(base_path, &format!("{}/{}", username, parent_path));
    let _ = tokio::fs::create_dir_all(&user_dir).await;
    let target_file_path = user_dir.join(&payload.file_name);

    let mut out_file = tokio::fs::File::create(&target_file_path)
        .await
        .map_err(|e| AppError::internal_log("创建目标文件", e))?;

    // RAII cleanup guard — 错误发生时自动清理 target_file + tmp_dir
    let mut cleanup = MergeCleanup::new(target_file_path.clone(), tmp_dir.clone());

    // 按顺序合并所有分片
    for idx in chunks {
        let chunk_path = format!("{}/{}", tmp_dir, idx);
        let mut chunk_file = tokio::fs::File::open(&chunk_path)
            .await
            .map_err(|e| AppError::internal_log(format!("读取分片 {idx}"), e))?;
        tokio::io::copy(&mut chunk_file, &mut out_file)
            .await
            .map_err(|e| AppError::internal_log(format!("合并分片 {idx}"), e))?;
    }
    let _ = out_file.flush().await;

    // 合并后清理临时分片目录和数据库记录
    let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    let _ = sqlx::query("DELETE FROM upload_chunks WHERE username = ? AND identifier = ?")
        .bind(username)
        .bind(&payload.identifier)
        .execute(&pool)
        .await;

    // 获取文件元数据
    let meta = target_file_path.metadata().map(|m| m.len()).unwrap_or(0);
    let file_size_mb_exact = meta as f64 / crate::handlers::utils::BYTES_PER_MB_F64;
    let file_size_mb_ceil = bytes_to_mb_ceil(meta);

    // 使用事务避免 TOCTOU 竞态：在事务内原子地检查并扣减配额
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| AppError::internal_log("开启配额事务", e))?;

    // 在事务内原子读取 used_mb + quota_mb
    let (current_used, quota): (i64, i64) =
        sqlx::query_as("SELECT used_mb, quota_mb FROM users WHERE username = ?")
            .bind(username)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| AppError::internal_log("查询用户配额", e))?
            .ok_or_else(|| AppError::not_found("用户不存在"))?;

    if current_used + file_size_mb_ceil > quota {
        return Err(AppError::forbidden(format!(
            "存储空间不足，配额 {} MB，已使用 {} MB",
            quota, current_used
        )));
    }

    // 完整文件 MIME 检测（安全增强）
    if !is_allowed_mime_streaming(&target_file_path)
        .await
        .map_err(|e| AppError::internal_log("文件完整性检测", e))?
    {
        return Err(AppError::forbidden("完整文件安全检测未通过：非法内容"));
    }

    // 在事务内插入文件记录并更新配额（原子操作）
    // size_mb 存精确值用于显示，配额用向上取整值
    sqlx::query(
        "INSERT INTO files (username, name, parent_path, is_dir, size_mb, identifier) VALUES (?, ?, ?, 0, ?, ?)"
    )
    .bind(username)
    .bind(&payload.file_name)
    .bind(&parent_path)
    .bind(file_size_mb_exact)
    .bind(&payload.identifier)
    .execute(&mut *tx)
    .await
    .map_err(|e| AppError::internal_log("文件记录入库", e))?;

    // 在事务内更新用户已用容量（配额用向上取整值避免微文件累积逃逸）
    sqlx::query("UPDATE users SET used_mb = MAX(0, used_mb + ?) WHERE username = ?")
        .bind(file_size_mb_ceil)
        .bind(username)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::internal_log("更新用户容量", e))?;

    tx.commit()
        .await
        .map_err(|e| AppError::internal_log("提交配额事务", e))?;

    // 提交成功 — 取消自动清理
    cleanup.disarm();

    // 审计日志：记录上传操作
    let target = if parent_path.is_empty() {
        payload.file_name.clone()
    } else {
        format!("{}/{}", parent_path, payload.file_name)
    };
    let details = format!("{:.2} MB", file_size_mb_exact);
    let _ = log_audit(
        &pool,
        username,
        "upload",
        Some(&target),
        Some(&details),
        None,
        None,
    )
    .await;

    Ok((StatusCode::OK, "文件上传合并成功"))
}
