// ====== Askama → Axum 集成层 ======
use askama::Template;
use axum::response::{Html, IntoResponse, Response};

/// Wrapper: 为所有 Askama 模板实现 Axum 的 `IntoResponse`
///
/// 用法：
/// ```ignore
/// #[derive(Template)]
/// #[template(path = "pages/home.html")]
/// struct HomePage { username: String, is_admin: bool, current_page: String }
///
/// async fn home() -> AppTemplate<HomePage> {
///     AppTemplate(HomePage { username: "admin".into(), is_admin: true, current_page: "home".into() })
/// }
/// ```
#[derive(Debug)]
pub struct AppTemplate<T: Template>(pub T);

impl<T: Template> IntoResponse for AppTemplate<T> {
    fn into_response(self) -> Response {
        match self.0.render() {
            Ok(html) => Html(html).into_response(),
            Err(e) => {
                tracing::error!("Template render error: {}", e);
                (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    format!("模板渲染错误: {}", e),
                )
                    .into_response()
            }
        }
    }
}

// ====== 所有页面模板共享的基础字段 ======
//
// 每个 extends "base.html" 的模板 struct 都需要包含这些字段：
//   pub username: String,
//   pub is_admin: bool,
//   pub current_page: String,  // "home"|"drive"|"todos"|"agent"|"links"|"trash"|"admin"

/// 导航页标识常量
pub const PAGE_HOME: &str = "home";
pub const PAGE_DRIVE: &str = "drive";
pub const PAGE_TODOS: &str = "todos";
pub const PAGE_AGENT: &str = "agent";
pub const PAGE_LINKS: &str = "links";
pub const PAGE_TRASH: &str = "trash";
pub const PAGE_ADMIN: &str = "admin";

/// 导航栏条目（base.html 通过 {% for item in nav %} 渲染）
#[derive(Debug)]
pub struct NavItem {
    pub href: &'static str,
    pub label: &'static str,
    pub page: &'static str,
}

/// 生成导航栏条目（admin 项按权限过滤），页面模板统一消费
pub fn nav_items(is_admin: bool) -> Vec<NavItem> {
    let mut items = vec![
        NavItem {
            href: "/",
            label: "首页",
            page: PAGE_HOME,
        },
        NavItem {
            href: "/drive",
            label: "云盘",
            page: PAGE_DRIVE,
        },
        NavItem {
            href: "/todos",
            label: "待办",
            page: PAGE_TODOS,
        },
        NavItem {
            href: "/agent",
            label: "AI",
            page: PAGE_AGENT,
        },
        NavItem {
            href: "/links",
            label: "链接",
            page: PAGE_LINKS,
        },
        NavItem {
            href: "/trash",
            label: "回收站",
            page: PAGE_TRASH,
        },
    ];
    if is_admin {
        items.push(NavItem {
            href: "/admin",
            label: "管理",
            page: PAGE_ADMIN,
        });
    }
    items
}

/// 为 HTML 元素的 class 生成 active 样式（用于 nav 链接）
#[macro_export]
macro_rules! nav_active {
    ($current:expr, $page:expr) => {
        if $current == $page {
            "bg-indigo-100 dark:bg-indigo-900/40 text-indigo-700 dark:text-indigo-300"
        } else {
            "text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700"
        }
    };
}
