// ====== 路由注册 ======
use axum::{
    extract::DefaultBodyLimit,
    routing::{get, post, put, delete},
    middleware,
    Extension, Router,
    http::{HeaderValue, header},
};
use tower_http::{
    compression::CompressionLayer,
    set_header::SetResponseHeaderLayer,
};

use crate::config::Config;
use crate::constants::*;
use crate::handlers;
use crate::middleware::csp::csp_middleware;

/// 构建完整的 Axum Router（注入 pool + config）
pub fn build_router(config: Config, pool: sqlx::SqlitePool) -> Router {
    // --- 公开路由（无需认证）---
    // Service Worker（必须从根路径服务）
    let sw_service = tower_http::services::ServeFile::new(format!("{}/sw.js", STATIC_DIR));

    let public_routes = Router::new()
        .route_service("/sw.js", sw_service)
        .route("/api/login", post(handlers::login))
        .route("/api/register", post(handlers::register))
        .route("/api/logout", post(handlers::logout))
        .route("/api/share/access/:code", get(handlers::access_share))
        .route("/s/:share_id", get(handlers::share_page))
        .route("/s/:share_id/*file_path", get(handlers::share_subfile))
        .route("/api/agent/models", get(handlers::get_models))
        .route("/health", get(handlers::health_check));

    // --- 受保护路由（需认证）---
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
        .route("/api/ssh/ws", get(handlers::ssh_ws_handler))
        .layer(middleware::from_fn(pinas_core::auth::auth_middleware));

    // --- 静态文件服务（24h 缓存） ---
    let assets_service = tower::ServiceBuilder::new()
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=86400"),
        ))
        .service(tower_http::services::ServeDir::new(ASSETS_DIR));

    // --- 组装 ---
    Router::new()
        .merge(public_routes)
        .merge(protected_routes)
        .nest_service("/assets", assets_service)
        .fallback_service(tower_http::services::ServeFile::new(format!("{}/index.html", STATIC_DIR)))
        .layer(CompressionLayer::new().gzip(true))
        .layer(DefaultBodyLimit::max(MAX_BODY_SIZE_BYTES))
        .layer(Extension(pool))
        .layer(Extension(config))
        .layer(middleware::from_fn(csp_middleware))
}
