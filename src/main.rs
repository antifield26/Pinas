mod handlers;
mod config;

use axum::{
    extract::DefaultBodyLimit,
    routing::{get, post, put, delete},
    middleware,
    Extension, Router,
};
use std::time::Duration;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};
use argon2::{
    password_hash::{PasswordHasher, SaltString, rand_core::OsRng},
    Argon2,
};
use tracing::info;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};
use crate::config::Config;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 加载配置
    let config = Config::from_env().expect("加载配置失败");
    info!("配置加载完成: {:?}", config);

    // 2. 创建日志目录并初始化日志
    tokio::fs::create_dir_all("logs").await?;
    let file_appender = tracing_appender::rolling::daily("logs", "app.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::registry()
        .with(fmt::Layer::new().with_writer(std::io::stdout).with_ansi(true))
        .with(fmt::Layer::new().with_writer(non_blocking).with_ansi(false))
        .with(EnvFilter::from_default_env())
        .init();

    // 3. 系统基础环境初始化
    if let Ok(data_dir) = std::env::var("PINAS_DATA_DIR") {
        if !data_dir.trim().is_empty() {
            let _ = std::env::set_current_dir(&data_dir);
        }
    }
    tokio::fs::create_dir_all("uploads/tmp/trash").await?;

    // 4. 数据库连接池配置 (高并发 WAL 模式)
    let db_url = &config.database_url;
    let connection_options = db_url.parse::<SqliteConnectOptions>()?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(10));

    let pool = sqlx::SqlitePool::connect_with(connection_options).await?;

    // ========== 数据库表初始化 ==========
    // 用户表
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL UNIQUE,
            password TEXT NOT NULL,
            role TEXT NOT NULL,
            quota_mb INTEGER,
            used_mb INTEGER DEFAULT 0
        )"
    )
    .execute(&pool).await?;

    // 会话表
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS sessions (
            token TEXT PRIMARY KEY,
            username TEXT NOT NULL,
            role TEXT NOT NULL,
            expires_at DATETIME NOT NULL
        )"
    ).execute(&pool).await?;

    // 文件索引表
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL,
            name TEXT NOT NULL,
            parent_path TEXT NOT NULL,
            is_dir INTEGER NOT NULL,
            size_mb TEXT NOT NULL,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(username, parent_path, name)
        )"
    ).execute(&pool).await?;

    // 分片上传临时记录表
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS upload_chunks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL,
            identifier TEXT NOT NULL,
            total_chunks INTEGER NOT NULL,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(username, identifier)
        )"
    ).execute(&pool).await?;

    // 分享表
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
    ).execute(&pool).await?;

    // 回收站表
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS trash (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL,
            original_path TEXT NOT NULL,
            trash_uuid TEXT NOT NULL,
            deleted_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )"
    ).execute(&pool).await?;

    // 审计日志表
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
    ).execute(&pool).await?;

    // 链接库表
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
    ).execute(&pool).await?;

    // 为已存在的表添加列（兼容性）
    sqlx::query("ALTER TABLE shares ADD COLUMN download_count INTEGER DEFAULT 0")
        .execute(&pool).await.ok();
    sqlx::query("ALTER TABLE users ADD COLUMN quota_mb INTEGER DEFAULT 0")
        .execute(&pool).await.ok();
    sqlx::query("ALTER TABLE users ADD COLUMN used_mb INTEGER DEFAULT 0")
        .execute(&pool).await.ok();

    // 创建索引
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_files_username_parent ON files (username, parent_path)")
        .execute(&pool).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_sessions_token ON sessions (token)")
        .execute(&pool).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_shares_code ON shares (code)")
        .execute(&pool).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_trash_username ON trash (username)")
        .execute(&pool).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_audit_username ON audit_logs (username)")
        .execute(&pool).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_audit_created_at ON audit_logs (created_at)")
        .execute(&pool).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_links_username ON links (username)")
        .execute(&pool).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_links_sort_order ON links (sort_order)")
        .execute(&pool).await?;

    // 清理旧格式的会话
    sqlx::query("DELETE FROM sessions").execute(&pool).await?;

    // 初始化默认管理员和访客
    let user_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&pool).await.unwrap_or(0);

    if user_count == 0 {
        fn hash_password(pwd: &str) -> Result<String, String> {
            let salt = SaltString::generate(&mut OsRng);
            Argon2::default()
                .hash_password(pwd.as_bytes(), &salt)
                .map(|hash| hash.to_string())
                .map_err(|e| format!("哈希失败: {}", e))
        }
        let admin_hash = hash_password("antifield")?;
        let guest_hash = hash_password("123456")?;

        sqlx::query("INSERT INTO users (username, password, role, quota_mb) VALUES (?, ?, ?, ?)")
            .bind("admin")
            .bind(&admin_hash)
            .bind("admin")
            .bind(config.default_quota_mb)
            .execute(&pool).await?;

        sqlx::query("INSERT INTO users (username, password, role, quota_mb) VALUES (?, ?, ?, ?)")
            .bind("guest")
            .bind(&guest_hash)
            .bind("user")
            .bind(config.default_quota_mb)
            .execute(&pool).await?;

        tokio::fs::create_dir_all("uploads/admin").await?;
        tokio::fs::create_dir_all("uploads/guest").await?;

        info!("✅ 已初始化默认账号：admin/antifield (管理员), guest/123456 (普通用户)");
    }

    // 启动时立即清理临时分片
    if let Err(e) = clean_expired_temp_chunks(&pool, config.temp_cleanup_hours).await {
        tracing::error!("初始清理临时分片失败: {}", e);
    }

    // 后台清理临时分片任务
    let temp_cleanup_hours = config.temp_cleanup_hours;
    let pool_clone = pool.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(temp_cleanup_hours * 3600));
        loop {
            interval.tick().await;
            let _ = clean_expired_temp_chunks(&pool_clone, temp_cleanup_hours).await;
        }
    });

    // 后台清理回收站任务
    let trash_cleanup_days = config.trash_cleanup_days;
    let cleanup_interval_hours = config.trash_cleanup_interval_hours;
    let pool_clone2 = pool.clone();
    tokio::spawn(async move {
        let interval_secs = cleanup_interval_hours * 3600;
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(e) = handlers::clean_expired_trash(&pool_clone2, trash_cleanup_days).await {
                tracing::error!("清理过期回收站失败: {}", e);
            } else {
                tracing::info!("已清理超过{}天的回收站记录", trash_cleanup_days);
            }
        }
    });

    // ========== 路由定义 ==========
    let public_routes = Router::new()
        .route("/api/login", post(handlers::login))
        .route("/api/register", post(handlers::register))
        .route("/api/logout", post(handlers::logout))
        .route("/api/share/access/:code", get(handlers::access_share))
        .route("/s/:share_id", get(handlers::share_page))
        .route("/s/:share_id/*file_path", get(handlers::share_subfile))
        .route("/health", get(handlers::health_check));

    let protected_routes = Router::new()
        .route("/api/files/list", get(handlers::list_files))
        .route("/api/files/create_folder", post(handlers::create_folder))
        .route("/api/files/check", get(handlers::check_chunk))
        .route("/api/files/upload_chunk", post(handlers::upload_chunk))
        .route("/api/files/merge", post(handlers::merge_chunks))
        .route("/api/files/delete", post(handlers::delete_item))
        .route("/api/files/rename", post(handlers::rename_item))
        .route("/api/files/move", post(handlers::move_item))
        .route("/api/move_batch", post(handlers::move_batch))
        .route("/api/files/download_zip", post(handlers::download_zip))
        .route("/api/edit/get", get(handlers::get_file_content_handler))
        .route("/api/edit/save", post(handlers::save_file_content_handler))
        .route("/api/system/status", get(handlers::get_system_status))
        .route("/api/share/create", post(handlers::create_share))
        .route("/api/share/list", get(handlers::list_shares))
        .route("/api/share/delete", post(handlers::delete_share))
        .route("/api/trash/list", get(handlers::list_trash))
        .route("/api/trash/restore", post(handlers::restore_trash))
        .route("/api/trash/delete", post(handlers::delete_trash_permanent))
        .route("/api/trash/clear", post(handlers::clear_trash))
        .route("/api/media/*path", get(handlers::media_proxy))
        .route("/api/admin/quota", get(handlers::get_user_quota))
        .route("/api/admin/quota", post(handlers::set_user_quota))
        .route("/api/admin/users", get(handlers::list_users))
        .route("/api/admin/user/reset_password", post(handlers::reset_user_password))
        .route("/api/admin/audit", get(handlers::get_audit_logs))
        .route("/api/links", get(handlers::get_links))
        .route("/api/links", post(handlers::create_link))
        .route("/api/links/:id", put(handlers::update_link))
        .route("/api/links/:id", delete(handlers::delete_link))
        .layer(middleware::from_fn(pinas_core::auth::auth_middleware));

    // 构建地址
    let addr = format!("{}:{}", config.server_host, config.server_port);
    
    // 构建应用，注入配置和连接池
    let app = Router::new()
        .merge(public_routes)
        .merge(protected_routes)
        .nest_service("/assets", tower_http::services::ServeDir::new("assets"))
        .fallback_service(tower_http::services::ServeFile::new("static/index.html"))
        .layer(DefaultBodyLimit::max((config.upload_limit_mb * 1024 * 1024) as usize))
        .layer(Extension(pool))
        .layer(Extension(config));

    // 启动 HTTP 服务
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("网盘核心服务已启动，监听: {}", addr);
    axum::serve(listener, app).await?;

    Ok(())
}

/// 清理过期临时分片（基于修改时间）
async fn clean_expired_temp_chunks(_pool: &sqlx::SqlitePool, hours: u64) -> Result<(), std::io::Error> {
    let tmp_dir = std::path::Path::new("uploads/tmp");
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