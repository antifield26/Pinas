// ====== HTMX 页面路由处理器 ======
use crate::core::UserSession;
use crate::templates::{AppTemplate, NavItem, nav_items};
use askama::Template;
use axum::{extract::Extension, response::IntoResponse};

// ====== 页面模板结构体（继承 base.html） ======

macro_rules! page_struct {
    ($name:ident, $path:literal) => {
        #[derive(Template)]
        #[template(path = $path)]
        struct $name {
            username: String,
            // 仅 home.html / admin.html 模板读取；其余页面由 nav_items() 在 Rust 侧过滤 admin 项
            #[allow(dead_code)]
            is_admin: bool,
            current_page: String,
            nav: Vec<NavItem>,
            // dsh Harness 入口（未配置 dsh_public_host 时为空串，导航不渲染）
            harness_url: String,
        }
    };
}

page_struct!(HomePage, "pages/home.html");
page_struct!(DrivePage, "pages/drive.html");
page_struct!(TodosPage, "pages/todos.html");
page_struct!(AgentPage, "pages/agent.html");
page_struct!(LinksPage, "pages/links.html");
page_struct!(TrashPage, "pages/trash.html");
page_struct!(AdminPage, "pages/admin.html");

/// 登录页面（独立模板）
#[derive(Template)]
#[template(path = "pages/login.html")]
struct LoginPage;

// ====== 辅助函数 ======

fn page_context(session: &UserSession, page: &str) -> (String, bool, String) {
    (
        session.username.clone(),
        session.role == "admin",
        page.to_string(),
    )
}

macro_rules! page_handler {
    ($func:ident, $PageType:ident, $page:expr) => {
        pub async fn $func(
            Extension(session): Extension<UserSession>,
            Extension(config): Extension<crate::config::Config>,
        ) -> impl IntoResponse {
            let (username, is_admin, current_page) = page_context(&session, $page);
            AppTemplate($PageType {
                username,
                is_admin,
                current_page,
                nav: nav_items(is_admin),
                harness_url: crate::templates::dsh_public_url(&config),
            })
        }
    };
}

// ====== 页面处理器 ======

page_handler!(home_page, HomePage, "home");
page_handler!(drive_page, DrivePage, "drive");
page_handler!(todos_page, TodosPage, "todos");
page_handler!(agent_page, AgentPage, "agent");
page_handler!(links_page, LinksPage, "links");
page_handler!(trash_page, TrashPage, "trash");

/// GET /admin — 管理页（仅 admin）：页面外壳同样 403——
/// 审计遗留项：历史用 page_handler! 宏，非管理员也能拿到完整外壳（结构信息泄漏）
pub async fn admin_page(
    Extension(session): Extension<UserSession>,
    Extension(config): Extension<crate::config::Config>,
) -> impl IntoResponse {
    if session.role != "admin" {
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }
    let (username, is_admin, current_page) = page_context(&session, "admin");
    AppTemplate(AdminPage {
        username,
        is_admin,
        current_page,
        nav: nav_items(is_admin),
        harness_url: crate::templates::dsh_public_url(&config),
    })
    .into_response()
}

/// 公开静态页面加边缘缓存头（Cloudflare 缓存，避免每次回源走慢链路）
fn with_public_cache(resp: axum::response::Response) -> axum::response::Response {
    let mut resp = resp;
    resp.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("public, max-age=60"),
    );
    resp
}

/// GET /login — 登录页面（公开）
#[tracing::instrument(skip_all)]
pub async fn login_page() -> impl IntoResponse {
    with_public_cache(AppTemplate(LoginPage).into_response())
}

/// 修改密码页面模板（独立模板，强制改密流程用）
#[derive(Template)]
#[template(path = "pages/change_password.html")]
struct ChangePasswordPage;

/// GET /change-password — 强制修改密码页面
#[tracing::instrument(skip_all)]
pub async fn change_password_page() -> impl IntoResponse {
    with_public_cache(AppTemplate(ChangePasswordPage).into_response())
}

/// 弹出式修改密码表单片段（导航「账号设置」加载进模态框）
#[derive(Template)]
#[template(path = "components/password_change_form.html")]
struct PasswordChangeFormFragment;

/// GET /account/password-form — 修改密码弹窗片段（登录用户）
#[tracing::instrument(skip_all)]
pub async fn password_form_fragment() -> impl IntoResponse {
    AppTemplate(PasswordChangeFormFragment)
}
