use axum::{
    extract::{Extension, Multipart, Query},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::handlers::utils::{
    is_allowed_mime,
    is_allowed_mime_streaming,
    is_blocked_extension,
    safe_join_sandbox,
    user_dir_path,
    log_audit,
    bytes_to_mb_ceil,
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

// --- 3. 文件分片秒传/断点续传检查 ---
pub async fn check_chunk(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Query(query): Query<CheckQuery>,
) -> impl IntoResponse {
    let username = &session.username;
    let identifier = &query.identifier;

    // 检查文件是否已完全上传（通过文件的 content identifier 匹配）
    let file_exists = sqlx::query("SELECT 1 FROM files WHERE username = ? AND identifier = ? LIMIT 1")
        .bind(username)
        .bind(identifier)
        .fetch_optional(&pool)
        .await
        .unwrap_or(None)
        .is_some();

    if file_exists {
        return Json(CheckResponse {
            exists: true,
            chunk_exists: None,
            uploaded_chunks: None,
        });
    }

    // 扫描临时分片目录，获取已上传的分片索引
    let tmp_dir = format!("uploads/tmp/{}", identifier);
    let mut uploaded_chunks = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&tmp_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if let Some(idx_str) = entry.file_name().to_str() {
                if let Ok(idx) = idx_str.parse::<i32>() {
                    uploaded_chunks.push(idx);
                }
            }
        }
    }
    uploaded_chunks.sort();

    Json(CheckResponse {
        exists: false,
        chunk_exists: None,
        uploaded_chunks: Some(uploaded_chunks),
    })
}

// --- 4. 处理分片上传（流式写入磁盘，避免内存缓冲） ---
use crate::constants::MAX_CHUNK_SIZE_BYTES as MAX_CHUNK_SIZE;

pub async fn upload_chunk(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Query(params): Query<ChunkParams>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    // 校验分片索引合法性
    if params.chunk_index < 0 || params.chunk_index >= params.total_chunks {
        return (StatusCode::BAD_REQUEST, "分片索引超出范围").into_response();
    }
    if params.total_chunks <= 0 || params.total_chunks > crate::constants::MAX_CHUNKS_PER_FILE {
        return (StatusCode::BAD_REQUEST, "总分片数不合法").into_response();
    }

    // 确保临时目录存在
    let tmp_dir = format!("uploads/tmp/{}", params.identifier);
    let _ = tokio::fs::create_dir_all(&tmp_dir).await;
    let chunk_path = format!("{}/{}", tmp_dir, params.chunk_index);

    // 直接创建文件，流式写入
    let mut file = match tokio::fs::File::create(&chunk_path).await {
        Ok(f) => f,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("创建分片文件失败: {}", e)).into_response(),
    };

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
                    return (StatusCode::PAYLOAD_TOO_LARGE, "分片超过 100 MB 上限").into_response();
                }

                // 首个分片：保留前 512 字节用于 MIME 检测
                if is_first_chunk && mime_read < crate::constants::MIME_HEADER_BUF_SIZE {
                    let to_copy = std::cmp::min(chunk.len(), crate::constants::MIME_HEADER_BUF_SIZE - mime_read);
                    mime_buf[mime_read..mime_read + to_copy].copy_from_slice(&chunk[..to_copy]);
                    mime_read += to_copy;
                }

                if let Err(e) = tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await {
                    let _ = tokio::fs::remove_file(&chunk_path).await;
                    return (StatusCode::INTERNAL_SERVER_ERROR, format!("写入分片失败: {}", e)).into_response();
                }
            }
        }
    }

    if total_written == 0 {
        let _ = tokio::fs::remove_file(&chunk_path).await;
        return (StatusCode::BAD_REQUEST, "分片数据流为空").into_response();
    }

    // 首分片 MIME 安全校验
    if is_first_chunk && !is_allowed_mime(&mime_buf[..mime_read]) {
        let _ = tokio::fs::remove_file(&chunk_path).await;
        return (StatusCode::FORBIDDEN, "非法执行文件伪装漏洞防护阻断").into_response();
    }

    // 记录总分数（若不存在则插入）
    let _ = sqlx::query(
        "INSERT OR IGNORE INTO upload_chunks (username, identifier, total_chunks) VALUES (?, ?, ?)"
    )
    .bind(&session.username)
    .bind(&params.identifier)
    .bind(params.total_chunks)
    .execute(&pool)
    .await;

    (StatusCode::OK, "分片暂存成功").into_response()
}

// --- 5. 分片合并（读取所有已上传分片，按索引排序合并）---
#[tracing::instrument(skip(pool, session, payload))]
pub async fn merge_chunks(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<MergeRequest>,
) -> impl IntoResponse {
    let username = &session.username;
    if is_blocked_extension(&payload.file_name) {
        return (StatusCode::FORBIDDEN, "安全策略阻断：不允许上传高危执行文件扩展名").into_response();
    }

    // 从数据库获取总分片数（可选，也可直接从文件系统推断）
    let total_chunks: Option<i32> = sqlx::query_scalar(
        "SELECT total_chunks FROM upload_chunks WHERE username = ? AND identifier = ?"
    )
    .bind(username)
    .bind(&payload.identifier)
    .fetch_optional(&pool)
    .await
    .unwrap_or(None);

    let tmp_dir = format!("uploads/tmp/{}", payload.identifier);
    // 读取目录下所有分片文件
    let mut chunks = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&tmp_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if let Some(idx_str) = entry.file_name().to_str() {
                if let Ok(idx) = idx_str.parse::<i32>() {
                    chunks.push(idx);
                }
            }
        }
    }
    if chunks.is_empty() {
        return (StatusCode::BAD_REQUEST, "未找到任何分片数据").into_response();
    }
    chunks.sort();

    // 如果已知总分片数，校验是否完整
    if let Some(total) = total_chunks {
        if chunks.len() != total as usize {
            return (StatusCode::BAD_REQUEST, format!(
                "分片不完整，已上传 {} 个，预期 {} 个", chunks.len(), total
            )).into_response();
        }
    }

    let parent_path = user_dir_path(Some(payload.parent_path));
    let base_path = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let user_dir = safe_join_sandbox(base_path, &format!("{}/{}", username, parent_path));
    let _ = tokio::fs::create_dir_all(&user_dir).await;
    let target_file_path = user_dir.join(&payload.file_name);

    let mut out_file = match tokio::fs::File::create(&target_file_path).await {
        Ok(f) => f,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("创建物理文件失败: {}", e)).into_response(),
    };

    // 按顺序合并所有分片
    for idx in chunks {
        let chunk_path = format!("{}/{}", tmp_dir, idx);
        let mut chunk_file = match tokio::fs::File::open(&chunk_path).await {
            Ok(f) => f,
            Err(e) => {
                let _ = tokio::fs::remove_file(&target_file_path).await;
                return (StatusCode::INTERNAL_SERVER_ERROR, format!("读取分片 {} 失败: {}", idx, e)).into_response();
            }
        };
        if let Err(e) = tokio::io::copy(&mut chunk_file, &mut out_file).await {
            let _ = tokio::fs::remove_file(&target_file_path).await;
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("合并分片 {} 失败: {}", idx, e)).into_response();
        }
    }
    let _ = out_file.flush().await;

    // 合并后清理临时分片目录
    let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    // 清理数据库记录
    let _ = sqlx::query("DELETE FROM upload_chunks WHERE username = ? AND identifier = ?")
        .bind(username)
        .bind(&payload.identifier)
        .execute(&pool)
        .await;

    // 获取文件元数据
    let meta = target_file_path.metadata().map(|m| m.len()).unwrap_or(0);
    let file_size_mb = bytes_to_mb_ceil(meta);

    // 使用事务避免 TOCTOU 竞态：在事务内原子地检查并扣减配额
    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            let _ = tokio::fs::remove_file(&target_file_path).await;
            let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("开启事务失败: {}", e)).into_response();
        }
    };

    // 在事务内原子读取 used_mb + quota_mb
    let (current_used, quota): (i64, i64) = match sqlx::query_as(
        "SELECT used_mb, quota_mb FROM users WHERE username = ?"
    )
    .bind(username)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            let _ = tokio::fs::remove_file(&target_file_path).await;
            let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
            return (StatusCode::NOT_FOUND, "用户不存在").into_response();
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&target_file_path).await;
            let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("查询用户配额失败: {}", e)).into_response();
        }
    };

    if current_used + file_size_mb > quota {
        let _ = tokio::fs::remove_file(&target_file_path).await;
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
        return (StatusCode::FORBIDDEN, format!("存储空间不足，配额 {} MB，已使用 {} MB", quota, current_used)).into_response();
    }

    // 完整文件 MIME 检测（安全增强）
    match is_allowed_mime_streaming(&target_file_path).await {
        Ok(true) => { /* 继续 */ }
        Ok(false) => {
            let _ = tokio::fs::remove_file(&target_file_path).await;
            return (StatusCode::FORBIDDEN, "完整文件安全检测未通过：非法内容").into_response();
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&target_file_path).await;
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("无法验证文件完整性: {}", e)).into_response();
        }
    }

    // 在事务内插入文件记录并更新配额（原子操作）
    if let Err(e) = sqlx::query(
        "INSERT INTO files (username, name, parent_path, is_dir, size_mb, identifier) VALUES (?, ?, ?, 0, ?, ?)"
    )
    .bind(username)
    .bind(&payload.file_name)
    .bind(&parent_path)
    .bind(file_size_mb)
    .bind(&payload.identifier)
    .execute(&mut *tx)
    .await
    {
        let _ = tokio::fs::remove_file(&target_file_path).await;
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("数据库录入失败: {}", e)).into_response();
    }

    // 在事务内更新用户已用容量
    if let Err(e) = sqlx::query("UPDATE users SET used_mb = MAX(0, used_mb + ?) WHERE username = ?")
        .bind(file_size_mb)
        .bind(username)
        .execute(&mut *tx)
        .await
    {
        let _ = tokio::fs::remove_file(&target_file_path).await;
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("更新用户容量失败: {}", e)).into_response();
    }

    if let Err(e) = tx.commit().await {
        let _ = tokio::fs::remove_file(&target_file_path).await;
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("提交事务失败: {}", e)).into_response();
    }

    // 审计日志：记录上传操作
    let target = if parent_path.is_empty() {
        payload.file_name.clone()
    } else {
        format!("{}/{}", parent_path, payload.file_name)
    };
    let details = format!("{} MB", file_size_mb);
    let _ = log_audit(&pool, username, "upload", Some(&target), Some(&details), None, None).await;

    (StatusCode::OK, "文件上传合并成功").into_response()
}