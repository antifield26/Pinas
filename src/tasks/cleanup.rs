// ====== 后台清理任务 ======
use sqlx::{Connection, SqlitePool};
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
    spawn_auto_backup(pool.clone(), cancel.child_token());
    spawn_wal_checkpoint(pool.clone(), cancel.child_token());
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

/// 自动备份（使用 VACUUM INTO 创建数据库副本）
fn spawn_auto_backup(pool: SqlitePool, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(86_400));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => { tracing::info!("自动备份任务已停止"); break; }
                _ = interval.tick() => {
                    let backup_dir = std::path::Path::new("backups");
                    if let Err(e) = tokio::fs::create_dir_all(backup_dir).await {
                        tracing::error!("创建备份目录失败: {}", e);
                        continue;
                    }
                    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
                    let backup_path = format!("backups/cloud_disk_backup_{}.db", ts);
                    // 备份前执行 WAL checkpoint 确保数据一致性
                    let _ = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)").execute(&pool).await;
                    match sqlx::SqliteConnection::connect_with(
                        &sqlx::sqlite::SqliteConnectOptions::new()
                            .filename(&backup_path)
                            .create_if_missing(true),
                    ).await {
                        Ok(mut conn) => {
                            let sql = format!("VACUUM INTO '{}'", backup_path);
                            if let Err(e) = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
                                .execute(&mut conn)
                                .await
                            {
                                tracing::error!("备份失败: {}", e);
                                let _ = tokio::fs::remove_file(&backup_path).await;
                            } else {
                                tracing::info!("自动备份完成: {}", backup_path);
                            }
                        }
                        Err(e) => tracing::error!("创建备份连接失败: {}", e),
                    }
                }
            }
        }
    });
}

// --- 辅助函数 ---

/// 清理超过指定小时数的临时分片
async fn clean_expired_temp_chunks(hours: u64) -> Result<(), std::io::Error> {
    let tmp_dir = std::path::Path::new(TMP_DIR);
    if !tmp_dir.exists() {
        return Ok(());
    }
    let mut entries = tokio::fs::read_dir(tmp_dir).await?;
    let now = std::time::SystemTime::now();
    let cutoff = now - std::time::Duration::from_secs(hours * 3600);
    while let Some(entry) = entries.next_entry().await? {
        let metadata = entry.metadata().await?;
        if let Ok(modified) = metadata.modified()
            && modified <= cutoff
        {
            let path = entry.path();
            if path.is_dir() {
                let _ = tokio::fs::remove_dir_all(&path).await;
                info!("清理过期临时分片目录: {:?}", path);
            } else if path.is_file() {
                let _ = tokio::fs::remove_file(&path).await;
                info!("清理过期临时文件: {:?}", path);
            }
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
