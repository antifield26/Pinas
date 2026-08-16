use axum::{
    extract::{Extension, Multipart, Query},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::core::UserSession;
use crate::error::{AppError, AppResult};
use crate::handlers::utils::{
    bytes_to_mb_ceil, is_allowed_mime, is_allowed_mime_streaming_sandbox, is_blocked_extension,
    log_audit,
};

// --- DTOs ---
#[derive(Deserialize)]
pub struct CheckQuery {
    pub identifier: String,
    /// 秒传目标校验（L5 修复）：客户端声明本次上传的目标文件路径。
    /// 缺省时保持兼容旧行为（按 identifier 命中即 exists）
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub parent_path: Option<String>,
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

/// 合并过程失败时自动清理临时文件和目录的 RAII guard。
/// P0-4：目标文件（用户路径）经 openat2 沙箱删除，绝不按字符串路径直接 unlink
struct MergeCleanup {
    target_rel: String,
    tmp_dir: String,
    armed: bool,
}

impl MergeCleanup {
    fn new(target_rel: String, tmp_dir: String) -> Self {
        Self {
            target_rel,
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
            if let Ok(sb) = crate::fsutil::Sandbox::new(crate::constants::UPLOADS_DIR) {
                let _ = sb.remove_file(&self.target_rel);
            }
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

    // 检查文件是否已完全上传（通过文件的 content identifier 匹配）。
    // L5 修复：identifier 只证明「内容曾在某路径存在过」——若目标路径不同，
    // 历史实现报 exists=true 让客户端跳过上传，结果目标文件从未被创建（假秒传）。
    // 客户端声明 file_name/parent_path 时必须在同路径命中才算秒传，否则走正常分片流程
    let file_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM files
            WHERE username = ? AND identifier = ?
              AND (? IS NULL OR name = ?)
              AND (? IS NULL OR parent_path = ?)
            LIMIT 1
        )",
    )
    .bind(username)
    .bind(identifier)
    .bind(&query.file_name)
    .bind(&query.file_name)
    .bind(&query.parent_path)
    .bind(&query.parent_path)
    .fetch_one(&pool)
    .await
    .unwrap_or(false);

    if file_exists {
        return Ok(Json(CheckResponse {
            exists: true,
            chunk_exists: None,
            uploaded_chunks: None,
        }));
    }

    // 扫描临时分片目录，获取已上传的分片索引（H2：目录按用户隔离，互不可见）
    let tmp_dir = format!(
        "{}/{}/{}",
        crate::constants::TMP_DIR,
        session.username,
        identifier
    );
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

    // 分片阶段磁盘上限（P1-4 原子化）：预检 + 预留并入同一写事务——
    // 历史实现「SELECT SUM 预检」与「写盘后累加」分离，并发请求可同时通过预检
    // 导致临时分片无限膨胀（5GB 上限形同虚设，系统盘 DoS）。
    // 现在：抢写锁 → 串行快照下预检 → 通过即按单片上限预留计数；写盘后按实际
    // 校正（-prev -MAX +actual），失败路径撤销预留（-prev -MAX，文件已删）。
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| AppError::internal_log("开启分片配额事务", e))?;
    // 抢占写锁：deferred 事务先读后写会锁升级竞态，空 UPDATE 直接拿写锁，
    // 保证 SUM 读到的是本事务线性化点之后的串行快照（并行请求在此排队）
    sqlx::query("UPDATE upload_chunks SET bytes_received = bytes_received WHERE username = ?")
        .bind(&session.username)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::internal_log("分片配额写锁", e))?;
    let pending_bytes: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(bytes_received), 0) FROM upload_chunks WHERE username = ?",
    )
    .bind(&session.username)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| AppError::internal_log("查询分片占用", e))?;
    if pending_bytes as u64 + crate::constants::MAX_CHUNK_SIZE_BYTES
        > crate::constants::PENDING_CHUNKS_CAP_BYTES
    {
        let _ = tx.rollback().await;
        return Err(AppError::too_many_requests(
            "临时分片存储超限，请先完成合并或等待清理",
        ));
    }
    // 登记总片数 + 预留单片上限（写盘后按实际校正；失败路径撤销）
    let _ = sqlx::query(
        "INSERT OR IGNORE INTO upload_chunks (username, identifier, total_chunks) VALUES (?, ?, ?)",
    )
    .bind(&session.username)
    .bind(&params.identifier)
    .bind(params.total_chunks)
    .execute(&mut *tx)
    .await;
    let _ = sqlx::query(
        "UPDATE upload_chunks SET bytes_received = bytes_received + ? WHERE username = ? AND identifier = ?",
    )
    .bind(crate::constants::MAX_CHUNK_SIZE_BYTES as i64)
    .bind(&session.username)
    .bind(&params.identifier)
    .execute(&mut *tx)
    .await;
    tx.commit()
        .await
        .map_err(|e| AppError::internal_log("分片配额预留提交", e))?;

    // 确保临时目录存在（H2：按用户隔离——历史实现全局共享 tmp/{identifier}，
    // 低权限用户可窃取/污染他人未合并上传、抢占 merge）
    let tmp_dir = format!(
        "{}/{}/{}",
        crate::constants::TMP_DIR,
        session.username,
        params.identifier
    );
    let _ = tokio::fs::create_dir_all(&tmp_dir).await;
    let chunk_path = format!("{}/{}", tmp_dir, params.chunk_index);

    // 重传语义：必须在截断之前读取旧大小——历史实现 create 之后才 metadata，
    // 读到的是新大小，UPDATE 恒为 bytes_received - N + N（5GB 上限形同虚设，H1 修复）
    let prev_chunk_len = tokio::fs::metadata(&chunk_path)
        .await
        .map(|m| m.len())
        .unwrap_or(0) as i64;

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
                    rollback_chunk_counter(
                        &pool,
                        &session.username,
                        &params.identifier,
                        params.chunk_index,
                        prev_chunk_len,
                    )
                    .await;
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
                    rollback_chunk_counter(
                        &pool,
                        &session.username,
                        &params.identifier,
                        params.chunk_index,
                        prev_chunk_len,
                    )
                    .await;
                    return Err(AppError::internal_log("写入分片", e));
                }
            }
        }
    }

    if total_written == 0 {
        let _ = tokio::fs::remove_file(&chunk_path).await;
        rollback_chunk_counter(
            &pool,
            &session.username,
            &params.identifier,
            params.chunk_index,
            prev_chunk_len,
        )
        .await;
        return Err(AppError::bad_request("分片数据流为空"));
    }

    // 首分片 MIME 安全校验
    if is_first_chunk && !is_allowed_mime(&mime_buf[..mime_read]) {
        let _ = tokio::fs::remove_file(&chunk_path).await;
        rollback_chunk_counter(
            &pool,
            &session.username,
            &params.identifier,
            params.chunk_index,
            prev_chunk_len,
        )
        .await;
        return Err(AppError::forbidden("非法执行文件伪装漏洞防护阻断"));
    }

    // 校正预留（P1-4）：预留按单片上限计入，写盘完成后替换为实际字节。
    // 公式：SUM -= prev（旧贡献，重传覆盖场景——旧大小已在写盘前计入 SUM）
    //     − MAX（撤销预留）+ actual（本次实际写盘）→ 净占用量精确反映磁盘
    // 同索引重传先截断再写：prev_chunk_len 在截断之前读取（H1）。
    let _ = sqlx::query(
        "UPDATE upload_chunks SET bytes_received = MAX(0, bytes_received - ? - ? + ?) WHERE username = ? AND identifier = ?",
    )
    .bind(prev_chunk_len)
    .bind(crate::constants::MAX_CHUNK_SIZE_BYTES as i64)
    .bind(total_written as i64)
    .bind(&session.username)
    .bind(&params.identifier)
    .execute(&pool)
    .await;

    // 记录该片实际字节数（merge 完整性校验依据，M3）：JSON map { "idx": bytes }
    let row: Option<String> = sqlx::query_scalar(
        "SELECT chunk_sizes FROM upload_chunks WHERE username = ? AND identifier = ?",
    )
    .bind(&session.username)
    .bind(&params.identifier)
    .fetch_optional(&pool)
    .await
    .unwrap_or(None);
    let mut sizes: serde_json::Map<String, serde_json::Value> = row
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    sizes.insert(
        params.chunk_index.to_string(),
        serde_json::json!(total_written),
    );
    let _ = sqlx::query(
        "UPDATE upload_chunks SET chunk_sizes = ? WHERE username = ? AND identifier = ?",
    )
    .bind(serde_json::to_string(&sizes).unwrap_or_default())
    .bind(&session.username)
    .bind(&params.identifier)
    .execute(&pool)
    .await;

    Ok((StatusCode::OK, "分片暂存成功"))
}

/// 分片文件被删除后的计数回滚：该片的旧贡献应从 bytes_received 扣除，
/// 并同步移除 chunk_sizes 中的尺寸记录
/// （写失败/超限/空流/MIME 阻断路径都会 remove_file，磁盘贡献归零）
async fn rollback_chunk_counter(
    pool: &sqlx::SqlitePool,
    username: &str,
    identifier: &str,
    chunk_index: i32,
    prev_len: i64,
) {
    // 撤销预留 + 旧贡献：分片文件已删除，磁盘贡献归零。
    // 预留按单片上限（MAX_CHUNK_SIZE_BYTES）计入（P1-4 预检原子化），
    // prev_len 为该片截断前的旧大小（SUM 中仍含）——两者一并扣回。
    let removed = crate::constants::MAX_CHUNK_SIZE_BYTES as i64 + prev_len.max(0);
    let _ = sqlx::query(
        "UPDATE upload_chunks SET bytes_received = MAX(0, bytes_received - ?) WHERE username = ? AND identifier = ?",
    )
    .bind(removed)
    .bind(username)
    .bind(identifier)
    .execute(pool)
    .await;
    let row: Option<String> = sqlx::query_scalar(
        "SELECT chunk_sizes FROM upload_chunks WHERE username = ? AND identifier = ?",
    )
    .bind(username)
    .bind(identifier)
    .fetch_optional(pool)
    .await
    .unwrap_or(None);
    if let Some(s) = row
        && let Ok(mut sizes) =
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&s)
    {
        sizes.remove(&chunk_index.to_string());
        let _ = sqlx::query(
            "UPDATE upload_chunks SET chunk_sizes = ? WHERE username = ? AND identifier = ?",
        )
        .bind(serde_json::to_string(&sizes).unwrap_or_default())
        .bind(username)
        .bind(identifier)
        .execute(pool)
        .await;
    }
}

// --- 5. 分片合并（读取所有已上传分片，按索引排序合并）---
#[tracing::instrument(skip(pool, session, payload))]
pub async fn merge_chunks(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<MergeRequest>,
) -> AppResult<(StatusCode, &'static str)> {
    let username = &session.username;
    // 文件名白名单校验：拒绝路径分隔符/穿越/引号/尖括号等（防止 join 逃逸沙箱）
    crate::handlers::utils::validate_name(&payload.file_name)?;
    if is_blocked_extension(&payload.file_name) {
        return Err(AppError::forbidden(
            "安全策略阻断：不允许上传高危执行文件扩展名",
        ));
    }
    validate_identifier(&payload.identifier)?;

    // 从数据库获取总分片数（DB 错误必须传播，否则静默跳过完整性校验）
    let total_chunks: Option<i32> = sqlx::query_scalar(
        "SELECT total_chunks FROM upload_chunks WHERE username = ? AND identifier = ?",
    )
    .bind(username)
    .bind(&payload.identifier)
    .fetch_optional(&pool)
    .await
    .map_err(|e| AppError::internal_log("查询分片记录", e))?;

    // H2：临时分片目录按用户隔离（与 check/upload 一致）
    let tmp_dir = format!(
        "{}/{}/{}",
        crate::constants::TMP_DIR,
        username,
        payload.identifier
    );
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

    // 如果已知总分片数，校验完整性：必须恰好是 0..total 的连续集合，
    // 仅数量相等会放过 {0,1,2,4} 这种缺 3 的空洞，产出错位损坏的文件
    if let Some(total) = total_chunks
        && chunks != (0..total).collect::<Vec<i32>>()
    {
        return Err(AppError::bad_request(format!(
            "分片不完整，已上传 {} 个，预期 {} 个",
            chunks.len(),
            total
        )));
    }

    // M3：分片尺寸校验——历史只校验索引连续，进程崩溃留下的截断分片会被
    // 静默合并成损坏文件。chunk_sizes 记录了每片上传时的实际字节数，逐一比对。
    if let Some(row) = sqlx::query_scalar::<_, String>(
        "SELECT chunk_sizes FROM upload_chunks WHERE username = ? AND identifier = ?",
    )
    .bind(username)
    .bind(&payload.identifier)
    .fetch_optional(&pool)
    .await
    .map_err(|e| AppError::internal_log("查询分片尺寸", e))?
        && let Ok(sizes) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&row)
    {
        for idx in &chunks {
            let expected = sizes.get(&idx.to_string()).and_then(|v| v.as_u64());
            let actual = tokio::fs::metadata(format!("{}/{}", tmp_dir, idx))
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            if let Some(exp) = expected
                && exp != actual
            {
                return Err(AppError::bad_request(format!(
                    "分片 {idx} 损坏或不完整（记录 {exp} 字节，实际 {actual} 字节），请重新上传该分片"
                )));
            }
        }
    }

    // P0-1：parent 逐段白名单校验（拒绝 `..` 跨用户路径；target_rel 随后仍过沙箱兜底）
    let parent_path = crate::handlers::utils::validate_rel_path(&payload.parent_path)?;
    // 目标目录可能尚未登记(文件夹上传到新子目录)：物理目录随后 create_dir_all,这里先补插目录行
    crate::handlers::file_ops::ensure_dir_rows(&pool, username, &parent_path).await?;
    let _base_path = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let target_rel = if parent_path.is_empty() {
        format!("{}/{}", username, payload.file_name)
    } else {
        format!("{}/{}/{}", username, parent_path, payload.file_name)
    };
    let user_dir_rel = if parent_path.is_empty() {
        username.clone()
    } else {
        format!("{}/{}", username, parent_path)
    };
    // openat2 沙箱：逐级创建父目录（用户可控路径，内核级越界防护）
    let sb = crate::fsutil::Sandbox::new(crate::constants::UPLOADS_DIR)
        .map_err(|e| AppError::internal_log("打开沙箱", e))?;
    sb.create_dir_all(&user_dir_rel)
        .map_err(|e| AppError::internal_log("创建目录", e))?;
    // 兜底复检：合并目标必须位于用户沙箱目录之下（防御未来校验绕过）
    if !target_rel.starts_with(&user_dir_rel) {
        return Err(AppError::bad_request("非法文件名"));
    }

    // 同名预检：File::create 会截断已存在文件，而随后 INSERT 因 UNIQUE 约束失败时
    // 清理守卫会连新文件一起删除，等于销毁了旧文件。必须先于任何写盘检查。
    let same_name_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM files WHERE username = ? AND name = ? AND parent_path = ?)",
    )
    .bind(username)
    .bind(&payload.file_name)
    .bind(&parent_path)
    .fetch_one(&pool)
    .await
    .unwrap_or(true); // DB 异常时保守拒绝，绝不冒险覆盖
    if same_name_exists || sb.try_exists(&target_rel).unwrap_or(true) {
        return Err(AppError::conflict("同名文件已存在，请先删除或重命名"));
    }

    // 合并前配额预检：先算分片总量再写盘，避免大文件写完才被拒绝
    let mut merged_size: u64 = 0;
    for idx in &chunks {
        if let Ok(m) = tokio::fs::metadata(format!("{}/{}", tmp_dir, idx)).await {
            merged_size += m.len();
        }
    }
    let (current_used, quota): (i64, i64) =
        sqlx::query_as("SELECT used_mb, quota_mb FROM users WHERE username = ?")
            .bind(username)
            .fetch_optional(&pool)
            .await
            .map_err(|e| AppError::internal_log("查询用户配额", e))?
            .ok_or_else(|| AppError::not_found("用户不存在"))?;
    if current_used + crate::handlers::utils::bytes_to_mb_ceil(merged_size) > quota {
        return Err(AppError::forbidden(format!(
            "存储空间不足，配额 {} MB，已使用 {} MB",
            quota, current_used
        )));
    }

    // O_EXCL 语义：同名预检后的原子创建（沙箱内，越界符号链接拒绝）
    let mut out_file = sb
        .open_write(&target_rel, false, true)
        .map(tokio::fs::File::from_std)
        .map_err(|e| AppError::internal_log("创建目标文件", e))?;

    // RAII cleanup guard — 错误发生时自动清理 target_file + tmp_dir
    let mut cleanup = MergeCleanup::new(target_rel.clone(), tmp_dir.clone());

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

    // 获取文件元数据（沙箱内，无二次路径解析）
    let meta = sb.metadata(&target_rel).map(|m| m.len()).unwrap_or(0);
    let file_size_mb_exact = meta as f64 / crate::handlers::utils::BYTES_PER_MB_F64;
    let file_size_mb_ceil = bytes_to_mb_ceil(meta);

    // M10：配额复核必须先于分片清理——历史顺序先删分片后复核，超配时用户
    // 连可重试的分片都没了。从此处起手工管理清理：任何失败只删目标文件、保留分片。
    cleanup.disarm();

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
        drop(tx);
        let _ = sb.remove_file(&target_rel);
        return Err(AppError::forbidden(format!(
            "存储空间不足，配额 {} MB，已使用 {} MB（分片已保留，清理空间后可重试合并）",
            quota, current_used
        )));
    }

    // 完整文件 MIME 检测（经沙箱打开，防符号链接指向外部文件）
    if !is_allowed_mime_streaming_sandbox(&sb, &target_rel)
        .map_err(|e| AppError::internal_log("文件完整性检测", e))?
    {
        drop(tx);
        let _ = sb.remove_file(&target_rel);
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
    crate::handlers::utils::adjust_user_used_mb_tx(&mut tx, username, file_size_mb_ceil)
        .await
        .map_err(|e| AppError::internal_log("更新用户容量", e))?;

    tx.commit()
        .await
        .map_err(|e| AppError::internal_log("提交配额事务", e))?;

    // 提交成功后清理临时分片目录和数据库记录（此刻才允许删除分片）
    let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
    let _ = sqlx::query("DELETE FROM upload_chunks WHERE username = ? AND identifier = ?")
        .bind(username)
        .bind(&payload.identifier)
        .execute(&pool)
        .await;

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
