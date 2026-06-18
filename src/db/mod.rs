// ====== 数据库初始化模块 ======
// 连接池创建、建表、索引、默认用户

mod migrations;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous, SqlitePoolOptions};
use std::time::Duration;
use tracing::info;

use crate::config::Config;
use crate::constants::*;
use crate::handlers::{hash_password, generate_random_password};

/// 创建 SQLite 连接池（WAL 模式，高并发读）
pub async fn create_pool(database_url: &str) -> Result<sqlx::SqlitePool, sqlx::Error> {
    let connection_options = database_url.parse::<SqliteConnectOptions>()?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(DB_BUSY_TIMEOUT_SECS));

    let pool = SqlitePoolOptions::new()
        .max_connections(DB_MAX_CONNECTIONS)
        .connect_with(connection_options).await?;

    sqlx::query("PRAGMA foreign_keys = ON").execute(&pool).await?;
    Ok(pool)
}

/// 初始化数据库：建表 → 迁移 → 索引 → 默认用户
pub async fn init(pool: &sqlx::SqlitePool, config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    init_tables(pool).await?;
    migrations::run(pool).await;
    init_indexes(pool).await?;
    init_default_users(pool, config).await?;
    // 清理过期会话
    sqlx::query("DELETE FROM sessions WHERE expires_at <= datetime('now')").execute(pool).await?;
    Ok(())
}

async fn init_tables(pool: &sqlx::SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL UNIQUE,
            password TEXT NOT NULL,
            role TEXT NOT NULL,
            quota_mb INTEGER,
            used_mb INTEGER DEFAULT 0
        )"
    ).execute(pool).await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS sessions (
            token TEXT PRIMARY KEY,
            username TEXT NOT NULL,
            role TEXT NOT NULL,
            expires_at DATETIME NOT NULL
        )"
    ).execute(pool).await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL,
            name TEXT NOT NULL,
            parent_path TEXT NOT NULL,
            is_dir INTEGER NOT NULL,
            size_mb REAL NOT NULL DEFAULT 0,
            identifier TEXT,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(username, parent_path, name)
        )"
    ).execute(pool).await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS upload_chunks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL,
            identifier TEXT NOT NULL,
            total_chunks INTEGER NOT NULL,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(username, identifier)
        )"
    ).execute(pool).await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS shares (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            code TEXT NOT NULL UNIQUE,
            file_path TEXT NOT NULL,
            is_dir INTEGER NOT NULL,
            username TEXT NOT NULL,
            expires_at DATETIME,
            password TEXT,
            has_password INTEGER DEFAULT 0,
            download_count INTEGER DEFAULT 0,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )"
    ).execute(pool).await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS trash (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL,
            original_path TEXT NOT NULL,
            trash_uuid TEXT NOT NULL,
            deleted_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )"
    ).execute(pool).await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS audit_logs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL,
            action TEXT NOT NULL,
            target TEXT,
            details TEXT,
            ip_address TEXT,
            user_agent TEXT,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )"
    ).execute(pool).await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS links (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL,
            title TEXT NOT NULL,
            url TEXT NOT NULL,
            icon TEXT,
            sort_order INTEGER DEFAULT 0,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY (username) REFERENCES users(username) ON DELETE CASCADE
        )"
    ).execute(pool).await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS todos (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL,
            title TEXT NOT NULL,
            description TEXT,
            due_date TEXT,
            is_all_day INTEGER NOT NULL DEFAULT 1,
            start_time TEXT,
            end_time TEXT,
            priority TEXT NOT NULL DEFAULT 'medium',
            status TEXT NOT NULL DEFAULT 'pending',
            category TEXT NOT NULL DEFAULT 'todo',
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY (username) REFERENCES users(username) ON DELETE CASCADE
        )"
    ).execute(pool).await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS user_settings (
            username TEXT PRIMARY KEY,
            deepseek_api_key TEXT,
            deepseek_api_base TEXT,
            deepseek_model TEXT,
            FOREIGN KEY (username) REFERENCES users(username) ON DELETE CASCADE
        )"
    ).execute(pool).await?;

    Ok(())
}

async fn init_indexes(pool: &sqlx::SqlitePool) -> Result<(), sqlx::Error> {
    let indexes: &[&str] = &[
        "CREATE INDEX IF NOT EXISTS idx_files_username_parent ON files (username, parent_path)",
        "CREATE INDEX IF NOT EXISTS idx_files_identifier ON files (identifier)",
        "CREATE INDEX IF NOT EXISTS idx_sessions_token ON sessions (token)",
        "CREATE INDEX IF NOT EXISTS idx_sessions_expires_at ON sessions (expires_at)",
        "CREATE INDEX IF NOT EXISTS idx_shares_code ON shares (code)",
        "CREATE INDEX IF NOT EXISTS idx_trash_username ON trash (username)",
        "CREATE INDEX IF NOT EXISTS idx_trash_deleted_at ON trash (deleted_at)",
        "CREATE INDEX IF NOT EXISTS idx_audit_username ON audit_logs (username)",
        "CREATE INDEX IF NOT EXISTS idx_audit_created_at ON audit_logs (created_at)",
        "CREATE INDEX IF NOT EXISTS idx_links_username ON links (username)",
        "CREATE INDEX IF NOT EXISTS idx_links_sort_order ON links (sort_order)",
        "CREATE INDEX IF NOT EXISTS idx_todos_username ON todos (username)",
        "CREATE INDEX IF NOT EXISTS idx_todos_category ON todos (category)",
        "CREATE INDEX IF NOT EXISTS idx_todos_due_date ON todos (due_date)",
        "CREATE INDEX IF NOT EXISTS idx_upload_chunks_identifier ON upload_chunks (identifier)",
    ];

    for idx in indexes {
        sqlx::query(idx).execute(pool).await?;
    }
    Ok(())
}

async fn init_default_users(pool: &sqlx::SqlitePool, config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let user_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(pool).await.unwrap_or(0);

    let is_first_run = user_count == 0;

    // --- 管理员密码 ---
    let admin_pwd = match config.admin_password.as_deref() {
        Some(p) if !p.is_empty() => {
            let pwd = p.to_string();
            // 环境变量明确设置了密码 → 始终更新数据库中的 admin 密码
            let hash = hash_password(&pwd)?;
            let existing: Option<String> = sqlx::query_scalar(
                "SELECT username FROM users WHERE username = ?"
            ).bind(ROLE_ADMIN).fetch_optional(pool).await?;
            if existing.is_some() {
                sqlx::query("UPDATE users SET password = ?, must_change_pwd = 0 WHERE username = ?")
                    .bind(&hash).bind(ROLE_ADMIN).execute(pool).await?;
                info!("[Init] PINAS_ADMIN_PASSWORD 已设置，admin 密码已同步更新");
            } else {
                sqlx::query("INSERT INTO users (username, password, role, quota_mb, must_change_pwd) VALUES (?, ?, ?, ?, 0)")
                    .bind(ROLE_ADMIN).bind(&hash).bind(ROLE_ADMIN).bind(config.default_quota_mb)
                    .execute(pool).await?;
            }
            pwd
        }
        _ => {
            if is_first_run {
                let pwd = generate_random_password();
                tracing::warn!("══════════════════════════════════════════════════");
                tracing::warn!("  未设置 PINAS_ADMIN_PASSWORD 环境变量");
                tracing::warn!("  已自动生成管理员随机密码: {}", pwd);
                tracing::warn!("  请立即登录并修改密码！");
                tracing::warn!("══════════════════════════════════════════════════");
                let hash = hash_password(&pwd)?;
                sqlx::query("INSERT INTO users (username, password, role, quota_mb, must_change_pwd) VALUES (?, ?, ?, ?, 1)")
                    .bind(ROLE_ADMIN).bind(&hash).bind(ROLE_ADMIN).bind(config.default_quota_mb)
                    .execute(pool).await?;
                pwd
            } else {
                String::new() // 非首次运行且未设置环境变量 → 不修改
            }
        }
    };

    // --- 访客密码 ---
    let guest_pwd = match config.guest_password.as_deref() {
        Some(p) if !p.is_empty() => {
            let pwd = p.to_string();
            let hash = hash_password(&pwd)?;
            let existing: Option<String> = sqlx::query_scalar(
                "SELECT username FROM users WHERE username = 'guest'"
            ).fetch_optional(pool).await?;
            if existing.is_some() {
                sqlx::query("UPDATE users SET password = ?, must_change_pwd = 0 WHERE username = 'guest'")
                    .bind(&hash).execute(pool).await?;
                info!("[Init] PINAS_GUEST_PASSWORD 已设置，guest 密码已同步更新");
            } else {
                sqlx::query("INSERT INTO users (username, password, role, quota_mb, must_change_pwd) VALUES ('guest', ?, ?, ?, 0)")
                    .bind(&hash).bind(ROLE_USER).bind(config.default_quota_mb)
                    .execute(pool).await?;
            }
            pwd
        }
        _ => {
            if is_first_run {
                let pwd = generate_random_password();
                tracing::warn!("  未设置 PINAS_GUEST_PASSWORD 环境变量");
                tracing::warn!("  已自动生成访客随机密码: {}", pwd);
                tracing::warn!("══════════════════════════════════════════════════");
                let hash = hash_password(&pwd)?;
                sqlx::query("INSERT INTO users (username, password, role, quota_mb, must_change_pwd) VALUES ('guest', ?, ?, ?, 1)")
                    .bind(&hash).bind(ROLE_USER).bind(config.default_quota_mb)
                    .execute(pool).await?;
                pwd
            } else {
                String::new()
            }
        }
    };

    if is_first_run {
        tokio::fs::create_dir_all(format!("{}/{}", UPLOADS_DIR, "admin")).await?;
        tokio::fs::create_dir_all(format!("{}/{}", UPLOADS_DIR, "guest")).await?;
        info!("✅ 已初始化默认账号（管理员+访客），可通过 PINAS_ADMIN_PASSWORD / PINAS_GUEST_PASSWORD 环境变量修改密码");
    }

    // 如果是自动生成的密码，写入文件作为备份
    let admin_is_auto = config.admin_password.as_deref().is_none_or(|p| p.is_empty());
    if admin_is_auto && is_first_run && !admin_pwd.is_empty() {
        let creds = format!("管理员: admin / {}\n访客: guest / {}\n", admin_pwd, guest_pwd);
        let _ = tokio::fs::write("credentials.txt", &creds).await;
        info!("自动生成的密码已保存到 credentials.txt（请修改密码后删除此文件）");
    }

    Ok(())
}

/// 测试专用：在内存数据库中创建所有表（不含默认用户，不创建文件系统目录）
pub async fn init_test_db(pool: &sqlx::SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query("PRAGMA foreign_keys = ON").execute(pool).await?;
    init_tables(pool).await?;
    migrations::run(pool).await;
    Ok(())
}
