// ====== 后台清理任务 ======
use std::time::Duration;
use tracing::info;

use crate::constants::*;
use crate::handlers;
use crate::handlers::rate_limit;

/// 启动所有后台清理任务
pub fn spawn_all(pool: &sqlx::SqlitePool, config: &crate::config::Config) {
    spawn_temp_chunk_cleanup(pool.clone(), config.temp_cleanup_hours);
    spawn_rate_limit_cleanup();
    spawn_log_cleanup();
    spawn_trash_cleanup(pool.clone(), config.trash_cleanup_days, config.trash_cleanup_interval_hours);
}

/// 清理过期临时分片
fn spawn_temp_chunk_cleanup(_pool: sqlx::SqlitePool, hours: u64) {
    tokio::spawn(async move {
        // 启动时立即清理
        if let Err(e) = clean_expired_temp_chunks(hours).await {
            tracing::error!("初始清理临时分片失败: {}", e);
        }
        let mut interval = tokio::time::interval(Duration::from_secs(hours * 3600));
        loop {
            interval.tick().await;
            if let Err(e) = clean_expired_temp_chunks(hours).await {
                tracing::error!("后台清理临时分片失败: {}", e);
            }
        }
    });
}

/// 清理过期日志（保留指定天数）
fn spawn_log_cleanup() {
    tokio::spawn(async move {
        // 启动时立即清理
        if let Err(e) = clean_old_logs(LOG_RETENTION_DAYS).await {
            tracing::error!("初始清理过期日志失败: {}", e);
        }
        let mut interval = tokio::time::interval(Duration::from_secs(LOG_CLEANUP_INTERVAL_SECS));
        loop {
            interval.tick().await;
            if let Err(e) = clean_old_logs(LOG_RETENTION_DAYS).await {
                tracing::error!("清理过期日志失败: {}", e);
            }
        }
    });
}

/// 清理速率限制过期条目
fn spawn_rate_limit_cleanup() {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(RATE_LIMIT_CLEANUP_INTERVAL_SECS));
        loop {
            interval.tick().await;
            rate_limit::clean_expired_entries(Duration::from_secs(RATE_LIMIT_CLEANUP_AGE_SECS));
        }
    });
}

/// 清理过期回收站条目
fn spawn_trash_cleanup(pool: sqlx::SqlitePool, days: u32, interval_hours: u64) {
    tokio::spawn(async move {
        // 启动时立即清理
        if let Err(e) = handlers::clean_expired_trash(&pool, days).await {
            tracing::error!("初始清理过期回收站失败: {}", e);
        } else {
            tracing::info!("初始清理：已清理超过{}天的回收站记录", days);
        }
        let interval_secs = interval_hours * 3600;
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        loop {
            interval.tick().await;
            if let Err(e) = handlers::clean_expired_trash(&pool, days).await {
                tracing::error!("清理过期回收站失败: {}", e);
            } else {
                tracing::info!("已清理超过{}天的回收站记录", days);
            }
        }
    });
}

/// 清理超过指定小时数的临时分片目录
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
        if let Ok(modified) = metadata.modified() {
            if modified <= cutoff {
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
    }
    Ok(())
}

/// 清理超过指定天数的日志文件
async fn clean_old_logs(retention_days: u64) -> Result<(), std::io::Error> {
    let log_dir = std::path::Path::new(LOGS_DIR);
    if !log_dir.exists() {
        return Ok(());
    }
    let cutoff = std::time::SystemTime::now()
        - std::time::Duration::from_secs(retention_days * 86_400);

    let mut entries = tokio::fs::read_dir(log_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().map_or(false, |e| e == "log") {
            if let Ok(metadata) = entry.metadata().await {
                if let Ok(modified) = metadata.modified() {
                    if modified < cutoff {
                        let _ = tokio::fs::remove_file(&path).await;
                        tracing::info!("已清理过期日志: {:?}", path);
                    }
                }
            }
        }
    }
    Ok(())
}
