// ====== Antifield Cloud (Pi-NAS) ======
// 自托管 NAS 网盘 + AI 助手
// 入口点：配置加载、日志初始化、数据库启动、路由注册、后台任务

use pi_nas::config::Config;
use pi_nas::constants::*;
use pi_nas::db;
use pi_nas::router;
use pi_nas::tasks;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 加载配置
    let config = Config::from_env().expect("加载配置失败");
    info!(
        "配置加载完成 (host={}, port={}, session_days={}, session_idle_minutes={})",
        config.server_host, config.server_port, config.session_days, config.session_idle_minutes
    );

    // 2. 初始化日志（文件 + 控制台）
    tokio::fs::create_dir_all(LOGS_DIR).await?;
    // L7：按天轮转 + 最多保留 30 份（历史无上限，异常日可无限堆积；tracing-appender 0.2
    // 无按大小轮转，以份数上限兜底磁盘占用）
    let file_appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("app.log")
        .max_log_files(30)
        .build(LOGS_DIR)
        .expect("构建日志滚动器失败");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::registry()
        .with(
            fmt::Layer::new()
                .with_writer(std::io::stdout)
                .with_ansi(true),
        )
        .with(fmt::Layer::new().with_writer(non_blocking).with_ansi(false))
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    // 3. 工作目录切换
    if let Ok(data_dir) = std::env::var("PINAS_DATA_DIR")
        && !data_dir.trim().is_empty()
        && let Err(e) = std::env::set_current_dir(&data_dir)
    {
        warn!("切换工作目录到 '{}' 失败: {}", data_dir, e);
    }
    // 4. 数据库连接池 + 初始化（含回收站旧目录迁移，必须先于 TRASH_DIR 创建与清扫任务）
    let pool = db::create_pool(&config.database_url).await?;
    db::init(&pool, &config).await?;
    tokio::fs::create_dir_all(TRASH_DIR).await?;

    // 4.5 文件操作意图日志重放（崩溃恢复）：必须先于后台清理任务——
    // 重放需要以崩溃时刻的磁盘/DB 状态为准，清扫任务不得先行改变现场
    pi_nas::handlers::replay_fs_journal(&pool).await;

    // 5. 创建全局取消令牌，并启动后台清理任务
    let cancel_token = CancellationToken::new();
    tasks::cleanup::spawn_all(&pool, &config, cancel_token.clone());

    // 6. 构建路由并启动 HTTP 服务（优雅关闭，支持 SIGINT + SIGTERM）
    let addr = format!("{}:{}", config.server_host, config.server_port);
    let app = router::build_router(config.clone(), pool.clone());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("网盘核心服务已启动，监听: {}", addr);

    // 6.5 第二监听：dsh 反代（仅绑定环回；cloudflared 本地接入，认证走 pinas 会话）
    if let Some(dsh_port) = config.dsh_port {
        let dsh_app = router::build_dsh_router(config.clone(), pool.clone());
        let dsh_listener = tokio::net::TcpListener::bind(("127.0.0.1", dsh_port)).await?;
        info!("dsh 反代已启动，监听: 127.0.0.1:{}", dsh_port);
        let dsh_shutdown = cancel_token.clone();
        tokio::spawn(async move {
            axum::serve(dsh_listener, dsh_app)
                .with_graceful_shutdown(async move {
                    dsh_shutdown.cancelled().await;
                })
                .await
                .expect("dsh 反代服务异常退出");
        });
    }

    let shutdown_token = cancel_token.clone();
    // 注入 ConnectInfo<SocketAddr>：auth 限速据此区分可信隧道(回环)与直连来源
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let sigint = tokio::signal::ctrl_c();
        let sigterm = async {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{SignalKind, signal};
                let mut stream = signal(SignalKind::terminate()).expect("无法注册 SIGTERM 处理器");
                stream.recv().await;
                info!("收到 SIGTERM 信号");
            }
            #[cfg(not(unix))]
            std::future::pending::<()>().await;
        };
        tokio::select! {
            _ = sigint => info!("收到 SIGINT (Ctrl-C) 信号"),
            _ = sigterm => {},
        }
        info!("正在优雅关闭...");
        // 通知后台任务停止
        shutdown_token.cancel();
        // 给予后台任务 5 秒完成清理
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    })
    .await?;

    info!("服务已安全关闭");
    Ok(())
}
