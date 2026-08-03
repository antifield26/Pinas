// ====== HTMX 页面路由处理器 ======
use askama::Template;
use axum::{extract::Extension, response::IntoResponse};
use pinas_core::UserSession;
use crate::templates::AppTemplate;

// ====== 页面模板结构体（继承 base.html） ======

macro_rules! page_struct {
    ($name:ident, $path:literal) => {
        #[derive(Template)]
        #[template(path = $path)]
        struct $name {
            username: String,
            is_admin: bool,
            current_page: String,
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

fn page_context(session: &UserSession, page: &str) -> impl FnOnce() -> (String, bool, String) {
    let username = session.username.clone();
    let is_admin = session.role == "admin";
    let current_page = page.to_string();
    move || (username, is_admin, current_page)
}

macro_rules! page_handler {
    ($func:ident, $PageType:ident, $page:expr) => {
        pub async fn $func(
            Extension(session): Extension<UserSession>,
        ) -> impl IntoResponse {
            let (username, is_admin, current_page) = page_context(&session, $page)();
            AppTemplate($PageType { username, is_admin, current_page })
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
page_handler!(admin_page, AdminPage, "admin");

/// GET /login — 登录页面（公开）
#[tracing::instrument(skip_all)]
pub async fn login_page() -> impl IntoResponse {
    AppTemplate(LoginPage)
}

/// 修改密码页面模板（独立模板）
#[derive(Template)]
#[template(path = "pages/change_password.html")]
struct ChangePasswordPage;

/// GET /change-password — 强制修改密码页面
#[tracing::instrument(skip_all)]
pub async fn change_password_page() -> impl IntoResponse {
    AppTemplate(ChangePasswordPage)
}
