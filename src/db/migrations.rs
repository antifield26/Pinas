// ====== 兼容性数据库迁移 ======
// 使用 ALTER TABLE ADD COLUMN 模式，列若已存在则静默忽略

use sqlx::SqlitePool;

/// 执行所有兼容性列迁移
pub async fn run(pool: &SqlitePool) {
    // files 表：identifier 列（秒传/断点续传文件指纹）
    if let Err(e) = sqlx::query("ALTER TABLE files ADD COLUMN identifier TEXT")
        .execute(pool).await {
        tracing::warn!("兼容性迁移 files.identifier 失败（若列已存在可忽略）: {}", e);
    }

    // todos 表：新列
    if let Err(e) = sqlx::query("ALTER TABLE todos ADD COLUMN is_all_day INTEGER NOT NULL DEFAULT 1")
        .execute(pool).await {
        tracing::warn!("兼容性迁移 todos.is_all_day 失败（若列已存在可忽略）: {}", e);
    }
    if let Err(e) = sqlx::query("ALTER TABLE todos ADD COLUMN start_time TEXT")
        .execute(pool).await {
        tracing::warn!("兼容性迁移 todos.start_time 失败（若列已存在可忽略）: {}", e);
    }
    if let Err(e) = sqlx::query("ALTER TABLE todos ADD COLUMN end_time TEXT")
        .execute(pool).await {
        tracing::warn!("兼容性迁移 todos.end_time 失败（若列已存在可忽略）: {}", e);
    }

    // shares 表：download_count 列
    if let Err(e) = sqlx::query("ALTER TABLE shares ADD COLUMN download_count INTEGER DEFAULT 0")
        .execute(pool).await {
        tracing::warn!("兼容性迁移 shares.download_count 失败（若列已存在可忽略）: {}", e);
    }

    // users 表：quota_mb, used_mb, must_change_pwd 列
    if let Err(e) = sqlx::query("ALTER TABLE users ADD COLUMN quota_mb INTEGER DEFAULT 0")
        .execute(pool).await {
        tracing::warn!("兼容性迁移 users.quota_mb 失败（若列已存在可忽略）: {}", e);
    }
    if let Err(e) = sqlx::query("ALTER TABLE users ADD COLUMN used_mb INTEGER DEFAULT 0")
        .execute(pool).await {
        tracing::warn!("兼容性迁移 users.used_mb 失败（若列已存在可忽略）: {}", e);
    }
    if let Err(e) = sqlx::query("ALTER TABLE users ADD COLUMN must_change_pwd INTEGER NOT NULL DEFAULT 0")
        .execute(pool).await {
        tracing::warn!("兼容性迁移 users.must_change_pwd 失败（若列已存在可忽略）: {}", e);
    }
}
