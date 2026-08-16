// ====== 路由注册 ======
use axum::{
    Extension, Router,
    extract::DefaultBodyLimit,
    http::{HeaderValue, header},
    middleware,
    routing::{delete, get, post, put},
};
use tower_http::{
    compression::{
        CompressionLayer,
        predicate::{DefaultPredicate, Predicate},
    },
    set_header::SetResponseHeaderLayer,
};

/// 压缩谓词：跳过 206 部分响应与媒体 MIME（压缩会破坏 Range 字节语义；
/// 且 Chromium 媒体管线拒绝任何带 Content-Encoding 的响应——SRC_NOT_SUPPORTED）
#[derive(Clone, Default)]
struct SkipStreamingCompression;

impl Predicate for SkipStreamingCompression {
    fn should_compress<B>(&self, response: &axum::http::Response<B>) -> bool
    where
        B: http_body::Body,
    {
        if response
            .headers()
            .contains_key(axum::http::header::CONTENT_RANGE)
        {
            return false;
        }
        if let Some(ct) = response.headers().get(axum::http::header::CONTENT_TYPE)
            && let Ok(ct) = ct.to_str()
        {
            let ct = ct.to_ascii_lowercase();
            if ct.starts_with("video/")
                || ct.starts_with("audio/")
                || ct.starts_with("text/event-stream")
            {
                return false;
            }
        }
        true
    }
}

use crate::config::Config;
use crate::constants::*;
use crate::handlers;
use crate::middleware::security_headers;

/// 构建 dsh 反代 Router（第二监听专用）
pub use handlers::build_dsh_router;

/// 构建完整的 Axum Router（注入 pool + config）
pub fn build_router(config: Config, pool: sqlx::SqlitePool) -> Router {
    let body_limit_bytes = (config.upload_limit_mb as usize) * 1024 * 1024;

    // --- 公开路由（无需认证）---
    // Service Worker（必须从根路径服务）
    let sw_service = tower_http::services::ServeFile::new(format!("{}/sw.js", STATIC_DIR));

    let public_routes = Router::new()
        .route_service("/sw.js", sw_service)
        .route("/login", get(handlers::login_page))
        .route("/change-password", get(handlers::change_password_page))
        .route("/api/login", post(handlers::login))
        .route("/api/user/password", post(handlers::change_password))
        .route("/api/register", post(handlers::register))
        .route("/api/logout", post(handlers::logout))
        .route("/api/share/access/{code}", get(handlers::access_share))
        .route("/s/{share_id}", get(handlers::share_page))
        .route("/s/{share_id}/{*file_path}", get(handlers::share_subfile))
        .route("/health", get(handlers::health_check));

    // --- 受保护路由（需认证）---
    let protected_routes = Router::new()
        // 账号设置弹窗片段（登录用户）
        .route(
            "/account/password-form",
            get(handlers::password_form_fragment),
        )
        // HTMX 页面路由
        .route("/", get(handlers::home_page))
        .route("/drive", get(handlers::drive_page))
        .route("/todos", get(handlers::todos_page))
        .route("/links", get(handlers::links_page))
        .route("/trash", get(handlers::trash_page))
        .route("/admin", get(handlers::admin_page))
        // 文件管理
        .route("/api/files/list", get(handlers::list_files))
        .route("/api/files/create_folder", post(handlers::create_folder))
        .route("/api/files/check", get(handlers::check_chunk))
        .route("/api/files/upload_chunk", post(handlers::upload_chunk))
        .route("/api/files/merge", post(handlers::merge_chunks))
        .route("/api/files/delete", post(handlers::delete_item))
        .route("/api/files/delete_batch", post(handlers::delete_batch))
        .route("/api/files/rename", post(handlers::rename_item))
        .route("/api/files/move", post(handlers::move_item))
        .route("/api/move_batch", post(handlers::move_batch))
        .route("/api/files/download_zip", post(handlers::download_zip))
        // HTMX Drive fragment routes
        .route("/drive/list", get(handlers::drive_list_fragment))
        .route(
            "/drive/breadcrumbs",
            get(handlers::drive_breadcrumbs_fragment),
        )
        .route("/drive/quota", get(handlers::drive_quota_fragment))
        .route("/drive/create-folder", post(handlers::drive_create_folder))
        .route("/drive/delete", post(handlers::drive_delete_item))
        .route("/drive/upload-form", get(handlers::drive_upload_form))
        .route(
            "/drive/new-folder-form",
            get(handlers::drive_new_folder_form),
        )
        .route("/drive/rename-form", get(handlers::drive_rename_form))
        .route("/drive/rename", post(handlers::drive_rename_item))
        .route("/drive/move-form", get(handlers::drive_move_form))
        .route("/drive/move", post(handlers::drive_move_item))
        .route("/drive/preview", get(handlers::drive_preview))
        // HTMX Trash fragment routes
        .route("/trash/list", get(handlers::trash_list_fragment))
        .route("/trash/clear", post(handlers::trash_clear_fragment))
        // HTMX Admin fragment routes
        .route("/admin/users", get(handlers::admin_users_fragment))
        .route("/api/edit/get", get(handlers::get_file_content_handler))
        .route("/api/edit/save", post(handlers::save_file_content_handler))
        .route("/api/system/status", get(handlers::get_system_status))
        .route(
            "/home/system-monitor",
            get(handlers::system_monitor_fragment),
        )
        .route("/api/minecraft/status", get(handlers::get_minecraft_status))
        .route(
            "/home/minecraft-status",
            get(handlers::minecraft_status_fragment),
        )
        .route("/api/share/create", post(handlers::create_share))
        .route("/api/share/list", get(handlers::list_shares))
        .route("/api/share/delete", post(handlers::delete_share))
        .route("/api/trash/list", get(handlers::list_trash))
        .route("/api/trash/restore", post(handlers::restore_trash))
        .route("/api/trash/delete", post(handlers::delete_trash_permanent))
        .route("/api/trash/clear", post(handlers::clear_trash))
        .route(
            "/api/media/{*path}",
            get(handlers::media_proxy).head(handlers::media_proxy),
        )
        .route("/api/admin/quota", get(handlers::get_user_quota))
        .route("/api/admin/quota", post(handlers::set_user_quota))
        .route("/api/admin/users", get(handlers::list_users))
        .route(
            "/api/admin/user/reset_password",
            post(handlers::reset_user_password),
        )
        .route("/api/admin/audit", get(handlers::get_audit_logs))
        .route("/api/admin/backup", post(handlers::create_backup))
        .route("/api/admin/backup/list", get(handlers::list_backups))
        .route("/api/admin/backup/download", get(handlers::download_backup))
        .route("/api/links", get(handlers::get_links))
        .route("/api/links", post(handlers::create_link))
        .route("/api/links/{id}", put(handlers::update_link))
        .route("/api/links/{id}", delete(handlers::delete_link))
        // HTMX Link fragment routes
        .route("/links/list", get(handlers::links_list_fragment))
        .route("/links", post(handlers::links_create_fragment))
        .route("/links/{id}", put(handlers::links_update_fragment))
        .route("/links/{id}", delete(handlers::links_delete_fragment))
        .route("/links/form", get(handlers::links_empty_form))
        .route("/links/form/{id}", get(handlers::links_edit_form))
        .route("/api/todos", get(handlers::get_todos))
        .route("/api/todos", post(handlers::create_todo))
        .route("/api/todos/{id}", put(handlers::update_todo))
        .route("/api/todos/{id}", delete(handlers::delete_todo))
        // HTMX Todo fragment routes
        .route("/todos/list", get(handlers::todos_list_fragment))
        .route("/todos/calendar", get(handlers::todos_calendar_fragment))
        .route("/todos", post(handlers::todos_create_fragment))
        .route("/todos/{id}", put(handlers::todos_update_fragment))
        .route("/todos/{id}", delete(handlers::todos_delete_fragment))
        .route("/todos/form", get(handlers::todos_empty_form))
        .route("/todos/form/{id}", get(handlers::todos_edit_form))
        // P1-9：认证策略显式声明（主路由：分享页公开 + 媒体令牌可用）
        .layer(middleware::from_fn_with_state(
            crate::core::auth::AuthPolicy::default(),
            crate::core::auth::auth_middleware,
        ));

    // --- WebDAV 路由（独立 Basic 认证，不走 cookie auth_middleware） ---
    // 路由级 body limit 覆盖全局 100MB：WebDAV PUT 大文件流式落盘
    // {*path} 不匹配空尾段，需同时注册 /dav 与 /dav/（根 PROPFIND 与客户端 OPTIONS 探测）
    let dav_body_limit = DefaultBodyLimit::max(5 * 1024 * 1024 * 1024);
    let dav_router = Router::new()
        .route(
            "/dav",
            axum::routing::any(handlers::dav_root_handler).layer(dav_body_limit),
        )
        .route(
            "/dav/",
            axum::routing::any(handlers::dav_root_handler).layer(dav_body_limit),
        )
        .route(
            "/dav/{*path}",
            axum::routing::any(handlers::dav_handler).layer(dav_body_limit),
        );

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
        .merge(dav_router)
        .nest_service("/assets", assets_service)
        .fallback(|| async { (axum::http::StatusCode::NOT_FOUND, "404 — 页面不存在") })
        .layer(
            CompressionLayer::new()
                .gzip(true)
                .compress_when(DefaultPredicate::new().and(SkipStreamingCompression)),
        )
        .layer(DefaultBodyLimit::max(body_limit_bytes))
        .layer(Extension(pool))
        .layer(Extension(config))
        .layer(middleware::from_fn(security_headers))
        // P1-2：X-Request-Id 最外层——所有响应（含 404 fallback）都带请求 ID
        .layer(middleware::from_fn(
            crate::middleware::request_id_middleware,
        ))
}
