// ====== Antifield Cloud (Pi-NAS) ======
// 自托管 NAS 网盘 + AI 助手
// 入口点：配置加载、日志初始化、数据库启动、路由注册、后台任务

mod config;
mod constants;
mod db;
mod error;
mod handlers;
mod middleware;
mod router;
mod tasks;

use crate::constants::*;
use crate::config::Config;
use tracing::{info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 加载配置
    let config = Config::from_env().expect("加载配置失败");
    info!("配置加载完成: {:?}", config);

    // 2. 初始化日志（文件 + 控制台）
    tokio::fs::create_dir_all(LOGS_DIR).await?;
    let file_appender = tracing_appender::rolling::daily(LOGS_DIR, "app.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::registry()
        .with(fmt::Layer::new().with_writer(std::io::stdout).with_ansi(true))
        .with(fmt::Layer::new().with_writer(non_blocking).with_ansi(false))
        .with(EnvFilter::from_default_env())
        .init();

    // 3. 工作目录切换
    if let Ok(data_dir) = std::env::var("PINAS_DATA_DIR") {
        if !data_dir.trim().is_empty() {
            if let Err(e) = std::env::set_current_dir(&data_dir) {
                warn!("切换工作目录到 '{}' 失败: {}", data_dir, e);
            }
        }
    }
    tokio::fs::create_dir_all(TRASH_DIR).await?;

    // 4. 数据库连接池 + 初始化
    let pool = db::create_pool(&config.database_url).await?;
    db::init(&pool, &config).await?;

    // 5. 启动后台清理任务
    tasks::cleanup::spawn_all(&pool, &config);

    // 6. 构建路由并启动 HTTP 服务
    let addr = format!("{}:{}", config.server_host, config.server_port);
    let app = router::build_router(config, pool);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("网盘核心服务已启动，监听: {}", addr);
    axum::serve(listener, app).await?;

    Ok(())
}
