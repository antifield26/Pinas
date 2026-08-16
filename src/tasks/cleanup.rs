// ====== 后台清理任务 ======
use sqlx::SqlitePool;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::constants::*;
use crate::handlers;
use crate::handlers::rate_limit;

/// 启动所有后台清理任务（接收 CancellationToken 用于优雅关闭协调）
pub fn spawn_all(
    pool: &sqlx::SqlitePool,
    config: &crate::config::Config,
    cancel: CancellationToken,
) {
    spawn_temp_chunk_cleanup(
        pool.clone(),
        config.temp_cleanup_hours,
        cancel.child_token(),
    );
    spawn_rate_limit_cleanup(cancel.child_token());
    spawn_log_cleanup(cancel.child_token());
    spawn_trash_cleanup(
        pool.clone(),
        config.trash_cleanup_days,
        config.trash_cleanup_interval_hours,
        cancel.child_token(),
    );
    spawn_session_cleanup(
        pool.clone(),
        config.session_idle_minutes,
        cancel.child_token(),
    );
    spawn_conversation_cleanup(pool.clone(), cancel.child_token());
    spawn_chunk_rows_cleanup(
        pool.clone(),
        config.temp_cleanup_hours,
        cancel.child_token(),
    );
    spawn_audit_cleanup(pool.clone(), cancel.child_token());
    spawn_auto_backup(pool.clone(), cancel.child_token());
    spawn_wal_checkpoint(pool.clone(), cancel.child_token());
}

/// 定期清理过期/超空闲会话行（sessions 表曾只在本启动时清理，运行期无限增长）
fn spawn_session_cleanup(pool: SqlitePool, idle_minutes: i64, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => { tracing::info!("会话清理任务已停止"); break; }
                _ = interval.tick() => {
                    let idle_mod = format!("-{} minutes", idle_minutes.max(1));
                    if let Err(e) = sqlx::query("DELETE FROM sessions WHERE expires_at <= datetime('now') OR last_active_at IS NULL OR last_active_at < datetime('now', ?)")
                        .bind(&idle_mod)
                        .execute(&pool).await
                    {
                        tracing::error!("清理过期会话失败: {}", e);
                    }
                    // 过期媒体令牌一并清理（短时效路径限定令牌，防表无限增长）
                    let _ = sqlx::query("DELETE FROM media_tokens WHERE expires_at <= datetime('now')")
                        .execute(&pool).await;
                    // AI 每日配额键清理（只保留本地时区今天）
                    crate::handlers::clean_agent_daily().await;
                }
            }
        }
    });
}

/// 每对话保留最近 500 条消息，超出的历史删除（防无界增长拖垮 LLM 上下文与响应体积）
const CONV_MESSAGE_CAP: i64 = 500;

fn spawn_conversation_cleanup(pool: SqlitePool, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => { tracing::info!("对话历史清理任务已停止"); break; }
                _ = interval.tick() => {
                    // L12 修复：相关子查询 LIMIT 500 是 O(rows×500) 的逐行扫描；
                    // 窗口函数一次排序即可算出每对话的保留边界
                    let r = sqlx::query(
                        "DELETE FROM conversation_messages WHERE id IN (
                            SELECT id FROM (
                                SELECT id,
                                    ROW_NUMBER() OVER (PARTITION BY conversation_id ORDER BY id DESC) AS rn
                                FROM conversation_messages
                            ) WHERE rn > ?
                        )",
                    )
                    .bind(CONV_MESSAGE_CAP)
                    .execute(&pool)
                    .await;
                    if let Err(e) = r {
                        tracing::error!("清理超限对话历史失败: {}", e);
                    }
                }
            }
        }
    });
}

/// 定期清理过期孤儿分片 DB 行。阈值与临时文件清扫（PINAS_TEMP_CLEANUP_HOURS）对齐——
/// M12 修复：历史固定 '-1 day'，管理员调短文件清扫周期后已删分片仍被计入 5GB 上限
fn spawn_chunk_rows_cleanup(pool: SqlitePool, hours: u64, cancel: CancellationToken) {
    tokio::spawn(async move {
        let hours = hours.max(1);
        let modifier = format!("-{} hours", hours);
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => { tracing::info!("分片记录清理任务已停止"); break; }
                _ = interval.tick() => {
                    if let Err(e) = sqlx::query(
                        "DELETE FROM upload_chunks WHERE created_at < datetime('now', ?)",
                    )
                    .bind(&modifier)
                    .execute(&pool)
                    .await
                    {
                        tracing::error!("清理孤儿分片记录失败: {}", e);
                    }
                }
            }
        }
    });
}

/// 审计日志保留 90 天（此前无清理，audit_logs 无限增长）
fn spawn_audit_cleanup(pool: SqlitePool, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(86_400));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => { tracing::info!("审计日志清理任务已停止"); break; }
                _ = interval.tick() => {
                    if let Err(e) = sqlx::query(
                        "DELETE FROM audit_logs WHERE created_at < datetime('now', '-90 days')",
                    ).execute(&pool).await
                    {
                        tracing::error!("清理过期审计日志失败: {}", e);
                    }
                }
            }
        }
    });
}

/// 定期执行 WAL checkpoint 防止 -wal 文件无限增长
fn spawn_wal_checkpoint(pool: SqlitePool, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => { tracing::info!("WAL checkpoint 任务已停止"); break; }
                _ = interval.tick() => {
                    if let Err(e) = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)").execute(&pool).await {
                        tracing::warn!("WAL checkpoint 失败: {}", e);
                    }
                }
            }
        }
    });
}

/// 清理过期临时分片
fn spawn_temp_chunk_cleanup(_pool: SqlitePool, hours: u64, cancel: CancellationToken) {
    // interval(0) 会 panic（period must be non-zero），release panic=abort 即整站启动崩溃
    let hours = hours.max(1);
    tokio::spawn(async move {
        if let Err(e) = clean_expired_temp_chunks(hours).await {
            tracing::error!("初始清理临时分片失败: {}", e);
        }
        let mut interval = tokio::time::interval(Duration::from_secs(hours * 3600));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => { tracing::info!("临时分片清理任务已停止"); break; }
                _ = interval.tick() => {
                    if let Err(e) = clean_expired_temp_chunks(hours).await {
                        tracing::error!("清理过期临时分片失败: {}", e);
                    }
                }
            }
        }
    });
}

/// 清理过期日志
fn spawn_log_cleanup(cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(LOG_CLEANUP_INTERVAL_SECS));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => { tracing::info!("日志清理任务已停止"); break; }
                _ = interval.tick() => {
                    if let Err(e) = clean_old_logs(LOG_RETENTION_DAYS).await {
                        tracing::error!("清理过期日志失败: {}", e);
                    }
                }
            }
        }
    });
}

/// 清理速率限制过期条目
fn spawn_rate_limit_cleanup(cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(Duration::from_secs(RATE_LIMIT_CLEANUP_INTERVAL_SECS));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => { tracing::info!("速率限制清理任务已停止"); break; }
                _ = interval.tick() => {
                    rate_limit::clean_expired_entries(Duration::from_secs(RATE_LIMIT_CLEANUP_AGE_SECS)).await;
                }
            }
        }
    });
}

/// 清理过期回收站条目
fn spawn_trash_cleanup(
    pool: SqlitePool,
    days: u32,
    interval_hours: u64,
    cancel: CancellationToken,
) {
    // interval(0) 会 panic（同 spawn_temp_chunk_cleanup 说明）
    let interval_hours = interval_hours.max(1);
    tokio::spawn(async move {
        if let Err(e) = handlers::clean_expired_trash(&pool, days).await {
            tracing::error!("初始清理过期回收站失败: {}", e);
        } else {
            tracing::info!("初始清理：已清理超过{}天的回收站记录", days);
        }
        let mut interval = tokio::time::interval(Duration::from_secs(interval_hours * 3600));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => { tracing::info!("回收站清理任务已停止"); break; }
                _ = interval.tick() => {
                    if let Err(e) = handlers::clean_expired_trash(&pool, days).await {
                        tracing::error!("清理过期回收站失败: {}", e);
                    } else {
                        tracing::info!("已清理超过{}天的回收站记录", days);
                    }
                }
            }
        }
    });
}

/// 自动备份（使用 VACUUM INTO 创建数据库副本）— 保留最近 7 份
fn spawn_auto_backup(pool: SqlitePool, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(86_400));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => { tracing::info!("自动备份任务已停止"); break; }
                _ = interval.tick() => {
                    match auto_backup_once(&pool, std::path::Path::new("backups")).await {
                        Ok(path) => tracing::info!("自动备份完成: {}", path),
                        Err(e) => tracing::error!("自动备份失败: {}", e),
                    }
                }
            }
        }
    });
}

/// 执行一次数据库备份(VACUUM INTO) + 保留轮转。
/// 注意:VACUUM INTO 必须在**源库连接**上执行(在备份文件连接上执行会自锁/产出空文件)。
pub async fn auto_backup_once(
    pool: &SqlitePool,
    backup_dir: &std::path::Path,
) -> Result<String, crate::error::AppError> {
    use crate::error::AppError;
    const BACKUP_KEEP: usize = 7;

    tokio::fs::create_dir_all(backup_dir)
        .await
        .map_err(|e| AppError::internal_log("创建备份目录", e))?;

    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let backup_path = backup_dir.join(format!("cloud_disk_backup_{}.db", ts));
    let backup_path_str = backup_path.to_string_lossy().to_string();
    if backup_path_str.contains('\'') {
        return Err(AppError::internal("备份路径非法"));
    }

    // 备份前执行 WAL checkpoint 确保数据一致性（失败不阻断备份）
    let _ = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(pool)
        .await;

    // 在源库池连接上执行 VACUUM INTO（路径为服务端生成的时间戳，无注入面）
    let sql = format!("VACUUM INTO '{}'", backup_path_str);
    sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
        .execute(pool)
        .await
        .map_err(|e| AppError::internal_log("VACUUM INTO 备份", e))?;

    // 保留轮转：只保留最新 BACKUP_KEEP 份，删除更早的
    let mut backups: Vec<(String, std::path::PathBuf)> = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(backup_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let fname = entry.file_name().to_string_lossy().to_string();
            if fname.starts_with("cloud_disk_backup_") && fname.ends_with(".db") {
                backups.push((fname.clone(), entry.path()));
            }
        }
    }
    backups.sort_by(|a, b| b.0.cmp(&a.0)); // 时间戳前缀字典序 = 时间倒序
    for (_, path) in backups.iter().skip(BACKUP_KEEP) {
        let _ = tokio::fs::remove_file(path).await;
    }

    Ok(backup_path_str)
}

// --- 辅助函数 ---

/// 判断 uploads/tmp 下的条目是否为可清扫的临时对象：
/// 分片目录（标识符 [A-Za-z0-9_-]+，见 upload.rs validate_identifier）或
/// WebDAV 临时文件（dav_{uuid} / dav_disp_{uuid}）。
/// 其余条目（如历史遗留的 trash 目录）一律跳过，防止误删非临时数据。
fn is_sweepable_temp_entry(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    // 纵深防御：即便回收站目录仍在 tmp 下（旧版遗留），也绝不清扫
    if name == "trash" {
        return false;
    }
    if name.starts_with("dav_") || name.starts_with("dav_disp_") {
        return true;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 清理超过指定小时数的临时分片（白名单：仅限分片目录与 dav 临时文件）
async fn clean_expired_temp_chunks(hours: u64) -> Result<(), std::io::Error> {
    clean_expired_temp_chunks_in(std::path::Path::new(TMP_DIR), hours).await
}

/// 同 clean_expired_temp_chunks，显式指定 tmp 目录（供测试注入临时路径，避免依赖进程 CWD）。
/// 支持两级结构（H2 修复后）：tmp/{username}/{identifier}/{chunk_idx}——
/// 用户名目录本身不按 mtime 整体判（写入已有分片不更新父目录 mtime），
/// 必须下钻到 identifier 目录逐个判定；兼容旧版扁平 tmp/{identifier} 结构。
async fn clean_expired_temp_chunks_in(
    tmp_dir: &std::path::Path,
    hours: u64,
) -> Result<(), std::io::Error> {
    if !tmp_dir.exists() {
        return Ok(());
    }
    let mut entries = tokio::fs::read_dir(tmp_dir).await?;
    let now = std::time::SystemTime::now();
    let cutoff = now - std::time::Duration::from_secs(hours * 3600);
    while let Some(entry) = entries.next_entry().await? {
        let metadata = entry.metadata().await?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if !is_sweepable_temp_entry(&name) {
            continue;
        }
        if path.is_file() {
            // WebDAV 临时文件（dav_{uuid}）：按自身 mtime 判
            if metadata.modified().is_ok_and(|m| m <= cutoff) {
                let _ = tokio::fs::remove_file(&path).await;
                info!("清理过期临时文件: {:?}", path);
            }
            continue;
        }
        // 目录：下钻一层，区分「用户名目录（含 identifier 子目录）」与「旧版扁平 identifier 目录」
        let Ok(mut sub) = tokio::fs::read_dir(&path).await else {
            continue;
        };
        let mut children = Vec::new();
        let mut has_subdirs = false;
        while let Ok(Some(c)) = sub.next_entry().await {
            let Ok(cmeta) = c.metadata().await else {
                continue;
            };
            if cmeta.is_dir() {
                has_subdirs = true;
            }
            children.push((c.path(), cmeta));
        }
        if has_subdirs {
            // 用户名目录：对每个子条目（identifier 分片目录）按 mtime 逐个判定
            for (cpath, cmeta) in children {
                if !cmeta.modified().is_ok_and(|m| m <= cutoff) {
                    continue;
                }
                if cmeta.is_dir() {
                    let _ = tokio::fs::remove_dir_all(&cpath).await;
                    info!("清理过期临时分片目录: {:?}", cpath);
                } else {
                    let _ = tokio::fs::remove_file(&cpath).await;
                    info!("清理过期临时文件: {:?}", cpath);
                }
            }
            // 子项清空后移除空壳用户名目录（非空则静默失败）
            let _ = tokio::fs::remove_dir(&path).await;
        } else if metadata.modified().is_ok_and(|m| m <= cutoff) {
            // 旧版扁平 identifier 目录（无子目录）：按自身 mtime 判
            let _ = tokio::fs::remove_dir_all(&path).await;
            info!("清理过期临时分片目录: {:?}", path);
        }
    }
    Ok(())
}

/// 清理超过指定天数的日志文件
async fn clean_old_logs(retention_days: u64) -> Result<(), std::io::Error> {
    let log_dir = std::path::Path::new(LOGS_DIR);
    if !log_dir.exists() {
        return Ok(());
    }
    let cutoff =
        std::time::SystemTime::now() - std::time::Duration::from_secs(retention_days * 86_400);
    let mut entries = tokio::fs::read_dir(log_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let metadata = entry.metadata().await?;
        if let Ok(modified) = metadata.modified()
            && modified <= cutoff
        {
            let path = entry.path();
            if path.is_file() {
                let _ = tokio::fs::remove_file(&path).await;
                info!("清理过期日志: {:?}", path);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// 回填旧 mtime（1 天前），使条目落入清扫窗口
    fn age_to_old(path: &std::path::Path) {
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(48 * 3600);
        filetime::set_file_mtime(path, old.into()).unwrap();
    }

    /// 回归测试：回收站目录（无论位置）与任何非分片条目都不得被临时清扫销毁
    #[tokio::test]
    async fn test_temp_sweep_spares_trash_and_unexpected_entries() {
        let dir = tempdir().unwrap();
        let tmp = dir.path();
        // 回收站（模拟旧版位于 tmp 内）
        let trash = tmp.join("trash");
        std::fs::create_dir_all(&trash).unwrap();
        std::fs::write(trash.join("victim.txt"), b"precious").unwrap();
        age_to_old(&trash);
        // 过期分片目录（应被清除）
        let chunk = tmp.join("abc-123_45");
        std::fs::create_dir_all(&chunk).unwrap();
        age_to_old(&chunk);
        // 过期 dav 临时目录（应被清除）
        let dav = tmp.join("dav_0000-1111-2222");
        std::fs::create_dir_all(&dav).unwrap();
        age_to_old(&dav);
        // 非分片模式的意外条目（带点号，不匹配白名单，应保留）
        let odd = tmp.join("keep.me");
        std::fs::write(&odd, b"not a chunk").unwrap();
        age_to_old(&odd);

        clean_expired_temp_chunks_in(tmp, 1).await.unwrap();

        assert!(trash.join("victim.txt").exists(), "回收站内容不得被清扫");
        assert!(!chunk.exists(), "过期分片目录应被清除");
        assert!(!dav.exists(), "过期 dav 临时目录应被清除");
        assert!(odd.exists(), "非分片条目应保留");
    }

    #[tokio::test]
    async fn test_auto_backup_creates_valid_db() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&db_path)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::query("CREATE TABLE IF NOT EXISTS t (id INTEGER)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO t VALUES (42)")
            .execute(&pool)
            .await
            .unwrap();

        let backup_dir = dir.path().join("backups");
        let backup_path = auto_backup_once(&pool, &backup_dir).await.unwrap();

        // 备份文件必须存在且包含源库的表与数据（验证 VACUUM INTO 在源库连接上正确执行）
        let check_pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&backup_path)
                    .read_only(true),
            )
            .await
            .unwrap();
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='t'",
        )
        .fetch_one(&check_pool)
        .await
        .unwrap();
        assert_eq!(n, 1, "备份文件应包含源库表");
        let v: i64 = sqlx::query_scalar("SELECT id FROM t")
            .fetch_one(&check_pool)
            .await
            .unwrap();
        assert_eq!(v, 42, "备份文件应包含源库数据");
    }
}
