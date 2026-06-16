mod handlers;
mod config;
mod constants;

use crate::constants::*;

use axum::{
    extract::DefaultBodyLimit,
    routing::{get, post, put, delete},
    middleware,
    Extension, Router,
    http::{HeaderValue, header},
    response::Response,
};
use tower_http::{
    compression::CompressionLayer,
    set_header::SetResponseHeaderLayer,
};
use std::time::Duration;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous, SqlitePoolOptions};
use tracing::info;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};
use crate::config::Config;
use crate::handlers::hash_password;
use crate::handlers::generate_random_password;
use crate::handlers::rate_limit;

/// CSP 中间件：为所有响应添加 Content-Security-Policy 头
async fn csp_middleware(req: axum::extract::Request, next: middleware::Next) -> Response {
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; \
             script-src 'self' 'unsafe-inline'; \
             style-src 'self' 'unsafe-inline'; \
             img-src 'self' data: blob:; \
             media-src 'self' blob:; \
             font-src 'self'; \
             connect-src 'self' blob:; \
             frame-src 'self'; \
             object-src 'none'; \
             base-uri 'self'; \
             form-action 'self'"
        ),
    );
    response
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 加载配置
    let config = Config::from_env().expect("加载配置失败");
    info!("配置加载完成: {:?}", config);

    // 2. 创建日志目录并初始化日志
    tokio::fs::create_dir_all(LOGS_DIR).await?;
    let file_appender = tracing_appender::rolling::daily(LOGS_DIR, "app.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::registry()
        .with(fmt::Layer::new().with_writer(std::io::stdout).with_ansi(true))
        .with(fmt::Layer::new().with_writer(non_blocking).with_ansi(false))
        .with(EnvFilter::from_default_env())
        .init();

    // 3. 系统基础环境初始化
    if let Ok(data_dir) = std::env::var("PINAS_DATA_DIR") {
        if !data_dir.trim().is_empty() {
            if let Err(e) = std::env::set_current_dir(&data_dir) {
                tracing::warn!("切换工作目录到 '{}' 失败: {}", data_dir, e);
            }
        }
    }
    tokio::fs::create_dir_all(TRASH_DIR).await?;

    // 4. 数据库连接池配置 (高并发 WAL 模式)
    let db_url = &config.database_url;
    let connection_options = db_url.parse::<SqliteConnectOptions>()?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(DB_BUSY_TIMEOUT_SECS));

    let pool = SqlitePoolOptions::new()
        .max_connections(DB_MAX_CONNECTIONS)
        .connect_with(connection_options).await?;

    // 启用外键约束
    sqlx::query("PRAGMA foreign_keys = ON").execute(&pool).await?;

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
            size_mb REAL NOT NULL DEFAULT 0,
            identifier TEXT,
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

    // 待办/日程表
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
    ).execute(&pool).await?;

    // 兼容性迁移：为已有 files 表添加 identifier 列（秒传/断点续传文件指纹）
    if let Err(e) = sqlx::query("ALTER TABLE files ADD COLUMN identifier TEXT")
        .execute(&pool).await {
        tracing::warn!("兼容性迁移 files.identifier 失败（若列已存在可忽略）: {}", e);
    }

    // 兼容性迁移：为已有 todos 表添加新列
    if let Err(e) = sqlx::query("ALTER TABLE todos ADD COLUMN is_all_day INTEGER NOT NULL DEFAULT 1")
        .execute(&pool).await {
        tracing::warn!("兼容性迁移 todos.is_all_day 失败（若列已存在可忽略）: {}", e);
    }
    if let Err(e) = sqlx::query("ALTER TABLE todos ADD COLUMN start_time TEXT")
        .execute(&pool).await {
        tracing::warn!("兼容性迁移 todos.start_time 失败（若列已存在可忽略）: {}", e);
    }
    if let Err(e) = sqlx::query("ALTER TABLE todos ADD COLUMN end_time TEXT")
        .execute(&pool).await {
        tracing::warn!("兼容性迁移 todos.end_time 失败（若列已存在可忽略）: {}", e);
    }

    // 待办/日程表完成后，创建用户设置表
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS user_settings (
            username TEXT PRIMARY KEY,
            deepseek_api_key TEXT,
            deepseek_api_base TEXT,
            deepseek_model TEXT,
            FOREIGN KEY (username) REFERENCES users(username) ON DELETE CASCADE
        )"
    ).execute(&pool).await?;

    // 为已存在的表添加列（兼容性：列已存在时忽略错误）
    if let Err(e) = sqlx::query("ALTER TABLE shares ADD COLUMN download_count INTEGER DEFAULT 0")
        .execute(&pool).await {
        tracing::warn!("兼容性迁移 shares.download_count 失败（若列已存在可忽略）: {}", e);
    }
    if let Err(e) = sqlx::query("ALTER TABLE users ADD COLUMN quota_mb INTEGER DEFAULT 0")
        .execute(&pool).await {
        tracing::warn!("兼容性迁移 users.quota_mb 失败（若列已存在可忽略）: {}", e);
    }
    if let Err(e) = sqlx::query("ALTER TABLE users ADD COLUMN used_mb INTEGER DEFAULT 0")
        .execute(&pool).await {
        tracing::warn!("兼容性迁移 users.used_mb 失败（若列已存在可忽略）: {}", e);
    }
    if let Err(e) = sqlx::query("ALTER TABLE users ADD COLUMN must_change_pwd INTEGER NOT NULL DEFAULT 0")
        .execute(&pool).await {
        tracing::warn!("兼容性迁移 users.must_change_pwd 失败（若列已存在可忽略）: {}", e);
    }

    // 创建索引
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_files_username_parent ON files (username, parent_path)")
        .execute(&pool).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_files_identifier ON files (identifier)")
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
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_todos_username ON todos (username)")
        .execute(&pool).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_todos_category ON todos (category)")
        .execute(&pool).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_todos_due_date ON todos (due_date)")
        .execute(&pool).await?;

    // 补充性能索引
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_sessions_expires_at ON sessions (expires_at)")
        .execute(&pool).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_trash_deleted_at ON trash (deleted_at)")
        .execute(&pool).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_upload_chunks_identifier ON upload_chunks (identifier)")
        .execute(&pool).await?;

    // 清理已过期的会话（保留有效会话，用户重启后无需重新登录）
    sqlx::query("DELETE FROM sessions WHERE expires_at <= datetime('now')").execute(&pool).await?;

    // 初始化默认管理员和访客
    let user_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&pool).await.unwrap_or(0);

    if user_count == 0 {
        // 管理员/访客密码：优先从环境变量读取，未设置则自动生成随机密码
        let admin_pwd = match config.admin_password.as_deref() {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => {
                let pwd = generate_random_password();
                tracing::warn!("══════════════════════════════════════════════════");
                tracing::warn!("  未设置 PINAS_ADMIN_PASSWORD 环境变量");
                tracing::warn!("  已自动生成管理员随机密码: {}", pwd);
                tracing::warn!("  请立即登录并修改密码！");
                tracing::warn!("══════════════════════════════════════════════════");
                pwd
            }
        };
        let guest_pwd = match config.guest_password.as_deref() {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => {
                let pwd = generate_random_password();
                tracing::warn!("  未设置 PINAS_GUEST_PASSWORD 环境变量");
                tracing::warn!("  已自动生成访客随机密码: {}", pwd);
                tracing::warn!("══════════════════════════════════════════════════");
                pwd
            }
        };
        // 标记是否为自动生成的密码（用于首次登录强制修改）
        let admin_must_change = if config.admin_password.as_deref().map_or(true, |p| p.is_empty()) { 1 } else { 0 };
        let guest_must_change = if config.guest_password.as_deref().map_or(true, |p| p.is_empty()) { 1 } else { 0 };

        let admin_hash = hash_password(&admin_pwd)?;
        let guest_hash = hash_password(&guest_pwd)?;

        sqlx::query("INSERT INTO users (username, password, role, quota_mb, must_change_pwd) VALUES (?, ?, ?, ?, ?)")
            .bind("admin")
            .bind(&admin_hash)
            .bind(ROLE_ADMIN)
            .bind(config.default_quota_mb)
            .bind(admin_must_change)
            .execute(&pool).await?;

        sqlx::query("INSERT INTO users (username, password, role, quota_mb, must_change_pwd) VALUES (?, ?, ?, ?, ?)")
            .bind("guest")
            .bind(&guest_hash)
            .bind(ROLE_USER)
            .bind(config.default_quota_mb)
            .bind(guest_must_change)
            .execute(&pool).await?;

        tokio::fs::create_dir_all(format!("{}/{}", UPLOADS_DIR, "admin")).await?;
        tokio::fs::create_dir_all(format!("{}/{}", UPLOADS_DIR, "guest")).await?;

        info!("✅ 已初始化默认账号（管理员+访客），可通过 PINAS_ADMIN_PASSWORD / PINAS_GUEST_PASSWORD 环境变量修改密码");
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
            if let Err(e) = clean_expired_temp_chunks(&pool_clone, temp_cleanup_hours).await {
                tracing::error!("后台清理临时分片失败: {}", e);
            }
        }
    });

    // 后台定期清理速率限制过期条目
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(RATE_LIMIT_CLEANUP_INTERVAL_SECS));
        loop {
            interval.tick().await;
            rate_limit::clean_expired_entries(Duration::from_secs(RATE_LIMIT_CLEANUP_AGE_SECS));
        }
    });

    // 后台清理过期日志（每日一次，保留 7 天）
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(LOG_CLEANUP_INTERVAL_SECS));
        loop {
            interval.tick().await;
            if let Err(e) = clean_old_logs(LOG_RETENTION_DAYS).await {
                tracing::error!("清理过期日志失败: {}", e);
            }
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
        .route("/api/agent/models", get(handlers::get_models))
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
        .route("/api/todos", get(handlers::get_todos))
        .route("/api/todos", post(handlers::create_todo))
        .route("/api/todos/:id", put(handlers::update_todo))
        .route("/api/todos/:id", delete(handlers::delete_todo))
        .route("/api/agent/chat", post(handlers::agent_chat))
        .route("/api/agent/briefing", post(handlers::generate_briefing))
        .route("/api/agent/settings", get(handlers::get_agent_settings))
        .route("/api/agent/settings", put(handlers::save_agent_settings))
        .layer(middleware::from_fn(pinas_core::auth::auth_middleware));

    // 构建地址
    let addr = format!("{}:{}", config.server_host, config.server_port);
    
    // 构建静态文件服务（带 24h 缓存）
    let assets_service = tower::ServiceBuilder::new()
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=86400"),
        ))
        .service(tower_http::services::ServeDir::new("assets"));

    // 构建应用，注入配置和连接池
    let app = Router::new()
        .merge(public_routes)
        .merge(protected_routes)
        .nest_service("/assets", assets_service)
        .fallback_service(tower_http::services::ServeFile::new("static/index.html"))
        .layer(CompressionLayer::new().gzip(true))
        .layer(DefaultBodyLimit::max(MAX_BODY_SIZE_BYTES))
        .layer(Extension(pool))
        .layer(Extension(config))
        .layer(middleware::from_fn(csp_middleware));

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

/// 清理超过指定天数的日志文件
async fn clean_old_logs(retention_days: u64) -> Result<(), std::io::Error> {
    let log_dir = std::path::Path::new("logs");
    if !log_dir.exists() {
        return Ok(());
    }
    let cutoff = std::time::SystemTime::now()
        - std::time::Duration::from_secs(retention_days * 86400);

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