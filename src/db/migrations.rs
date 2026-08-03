// ====== 版本化数据库迁移 ======
// 使用 schema_version 表追踪已应用的迁移，避免重复执行

use sqlx::{Row, SqlitePool};

/// 当前最新 schema 版本号
const CURRENT_VERSION: i32 = 3;

/// 执行所有待迁移的 schema 变更
pub async fn run(pool: &SqlitePool) {
    // 确保版本表存在
    sqlx::query("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY)")
        .execute(pool)
        .await
        .expect("创建 schema_version 表失败");

    // 获取当前版本
    let current: i32 = sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) FROM schema_version")
        .fetch_one(pool)
        .await
        .unwrap_or(0);

    // 按版本号递增执行迁移
    if current < 1 {
        apply_v1_migrations(pool).await;
        sqlx::query("INSERT INTO schema_version (version) VALUES (1)")
            .execute(pool)
            .await
            .expect("记录 schema v1 失败");
        tracing::info!("[DB] Schema 迁移到 v1 完成");
    }

    if current < 2 {
        apply_v2_migrations(pool).await;
        sqlx::query("INSERT INTO schema_version (version) VALUES (2)")
            .execute(pool)
            .await
            .expect("记录 schema v2 失败");
        tracing::info!("[DB] Schema 迁移到 v2 完成");
    }

    if current < 3 {
        apply_v3_migrations(pool).await;
        sqlx::query("INSERT INTO schema_version (version) VALUES (3)")
            .execute(pool)
            .await
            .expect("记录 schema v3 失败");
        tracing::info!("[DB] Schema 迁移到 v3 完成");
    }

    tracing::debug!(
        "[DB] Schema 版本: {} (最新: {})",
        current.max(CURRENT_VERSION),
        CURRENT_VERSION
    );
}

/// v1: 初始兼容性列迁移（仅对缺少列的表添加，检查 PRAGMA table_info）
async fn apply_v1_migrations(pool: &SqlitePool) {
    // files 表：identifier 列
    let cols: Vec<String> = sqlx::query("PRAGMA table_info(files)")
        .fetch_all(pool)
        .await
        .map(|rows| rows.iter().filter_map(|r| r.try_get("name").ok()).collect())
        .unwrap_or_default();
    if !cols.iter().any(|c| c == "identifier") {
        sqlx::query("ALTER TABLE files ADD COLUMN identifier TEXT")
            .execute(pool)
            .await
            .expect("迁移 files.identifier 失败");
    }

    // todos 表：新列
    let todo_cols: Vec<String> = sqlx::query("PRAGMA table_info(todos)")
        .fetch_all(pool)
        .await
        .map(|rows| rows.iter().filter_map(|r| r.try_get("name").ok()).collect())
        .unwrap_or_default();
    if !todo_cols.iter().any(|c| c == "is_all_day") {
        sqlx::query("ALTER TABLE todos ADD COLUMN is_all_day INTEGER NOT NULL DEFAULT 1")
            .execute(pool)
            .await
            .expect("迁移 todos.is_all_day 失败");
    }
    if !todo_cols.iter().any(|c| c == "start_time") {
        sqlx::query("ALTER TABLE todos ADD COLUMN start_time TEXT")
            .execute(pool)
            .await
            .expect("迁移 todos.start_time 失败");
    }
    if !todo_cols.iter().any(|c| c == "end_time") {
        sqlx::query("ALTER TABLE todos ADD COLUMN end_time TEXT")
            .execute(pool)
            .await
            .expect("迁移 todos.end_time 失败");
    }

    // shares 表：download_count 列
    let share_cols: Vec<String> = sqlx::query("PRAGMA table_info(shares)")
        .fetch_all(pool)
        .await
        .map(|rows| rows.iter().filter_map(|r| r.try_get("name").ok()).collect())
        .unwrap_or_default();
    if !share_cols.iter().any(|c| c == "download_count") {
        sqlx::query("ALTER TABLE shares ADD COLUMN download_count INTEGER DEFAULT 0")
            .execute(pool)
            .await
            .expect("迁移 shares.download_count 失败");
    }

    // users 表：quota_mb, used_mb, must_change_pwd 列
    let user_cols: Vec<String> = sqlx::query("PRAGMA table_info(users)")
        .fetch_all(pool)
        .await
        .map(|rows| rows.iter().filter_map(|r| r.try_get("name").ok()).collect())
        .unwrap_or_default();
    if !user_cols.iter().any(|c| c == "quota_mb") {
        sqlx::query("ALTER TABLE users ADD COLUMN quota_mb INTEGER DEFAULT 0")
            .execute(pool)
            .await
            .expect("迁移 users.quota_mb 失败");
    }
    if !user_cols.iter().any(|c| c == "used_mb") {
        sqlx::query("ALTER TABLE users ADD COLUMN used_mb INTEGER DEFAULT 0")
            .execute(pool)
            .await
            .expect("迁移 users.used_mb 失败");
    }
    if !user_cols.iter().any(|c| c == "must_change_pwd") {
        sqlx::query("ALTER TABLE users ADD COLUMN must_change_pwd INTEGER NOT NULL DEFAULT 0")
            .execute(pool)
            .await
            .expect("迁移 users.must_change_pwd 失败");
    }
}

/// v2: 清理 files.size_mb 列混合类型（TEXT → REAL）
/// SQLite CAST: '-' → 0.0, '0.50' → 0.5, 已有的 REAL 值不变
async fn apply_v2_migrations(pool: &SqlitePool) {
    let affected = sqlx::query(
        "UPDATE files SET size_mb = CAST(size_mb AS REAL) WHERE typeof(size_mb) = 'text'",
    )
    .execute(pool)
    .await
    .map(|r| r.rows_affected())
    .unwrap_or(0);
    tracing::info!("[DB] v2 迁移: size_mb 列 {affected} 行 TEXT → REAL 已转换");
}

/// v3: user_settings 添加 temperature 和 max_tokens 列
async fn apply_v3_migrations(pool: &SqlitePool) {
    let cols: Vec<String> = sqlx::query("PRAGMA table_info(user_settings)")
        .fetch_all(pool)
        .await
        .unwrap_or_default()
        .iter()
        .map(|r| r.get::<String, _>(1))
        .collect();

    if !cols.iter().any(|c| c == "temperature") {
        sqlx::query("ALTER TABLE user_settings ADD COLUMN temperature REAL NOT NULL DEFAULT 0.7")
            .execute(pool)
            .await
            .expect("迁移 user_settings.temperature 失败");
    }
    if !cols.iter().any(|c| c == "max_tokens") {
        sqlx::query(
            "ALTER TABLE user_settings ADD COLUMN max_tokens INTEGER NOT NULL DEFAULT 4096",
        )
        .execute(pool)
        .await
        .expect("迁移 user_settings.max_tokens 失败");
    }
    tracing::info!("[DB] v3 迁移: user_settings 添加 temperature 和 max_tokens 列");
}
