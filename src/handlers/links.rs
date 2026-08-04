use axum::{
    extract::{Extension, Path},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::core::UserSession;
use crate::error::{AppError, AppResult};
use crate::handlers::utils::log_audit;

// DTOs
#[derive(Debug, Deserialize)]
pub struct CreateLinkRequest {
    pub title: String,
    pub url: String,
    pub icon: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateLinkRequest {
    pub title: Option<String>,
    pub url: Option<String>,
    pub icon: Option<String>,
    pub sort_order: Option<i32>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct LinkItem {
    pub id: i64,
    pub title: String,
    pub url: String,
    pub icon: Option<String>,
    pub sort_order: i32,
    pub created_at: String,
}

/// GET /api/links — 获取当前用户的所有链接
#[tracing::instrument(skip_all)]
pub async fn get_links(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> AppResult<Json<Vec<LinkItem>>> {
    let links = sqlx::query_as::<_, LinkItem>(
        "SELECT id, title, url, icon, sort_order, created_at FROM links WHERE username = ? ORDER BY sort_order, id"
    )
    .bind(&session.username)
    .fetch_all(&pool)
    .await?;
    Ok(Json(links))
}

/// POST /api/links — 添加新链接
#[tracing::instrument(skip_all)]
pub async fn create_link(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<CreateLinkRequest>,
) -> AppResult<(StatusCode, &'static str)> {
    if payload.title.trim().is_empty() || payload.url.trim().is_empty() {
        return Err(AppError::bad_request("标题和URL不能为空"));
    }
    if !payload.url.starts_with("http://") && !payload.url.starts_with("https://") {
        return Err(AppError::bad_request("URL必须以http://或https://开头"));
    }
    sqlx::query("INSERT INTO links (username, title, url, icon) VALUES (?, ?, ?, ?)")
        .bind(&session.username)
        .bind(&payload.title)
        .bind(&payload.url)
        .bind(&payload.icon)
        .execute(&pool)
        .await?;
    let _ = log_audit(
        &pool,
        &session.username,
        "create_link",
        Some(&payload.title),
        None,
        None,
        None,
    )
    .await;
    Ok((StatusCode::CREATED, "链接添加成功"))
}

/// PUT /api/links/:id — 更新链接（使用 COALESCE 避免动态 SQL 拼接）
#[tracing::instrument(skip_all)]
pub async fn update_link(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Path(id): Path<i64>,
    Json(payload): Json<UpdateLinkRequest>,
) -> AppResult<(StatusCode, &'static str)> {
    if let Some(ref url) = payload.url
        && !url.starts_with("http://")
        && !url.starts_with("https://")
    {
        return Err(AppError::bad_request("URL必须以http://或https://开头"));
    }
    // 不允许将标题更新为空字符串
    if let Some(ref t) = payload.title
        && t.trim().is_empty()
    {
        return Err(AppError::bad_request("标题不能为空"));
    }
    if payload.title.as_ref().is_none_or(|t| t.trim().is_empty())
        && payload.url.is_none()
        && payload.icon.is_none()
        && payload.sort_order.is_none()
    {
        return Err(AppError::bad_request("没有要更新的字段"));
    }

    let result = sqlx::query(
        "UPDATE links SET title = COALESCE(?, title), url = COALESCE(?, url), icon = COALESCE(?, icon), sort_order = COALESCE(?, sort_order) WHERE id = ? AND username = ?"
    )
    .bind(&payload.title)
    .bind(&payload.url)
    .bind(&payload.icon)
    .bind(payload.sort_order)
    .bind(id)
    .bind(&session.username)
    .execute(&pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::not_found("记录不存在或无权操作"));
    }

    let _ = log_audit(
        &pool,
        &session.username,
        "update_link",
        Some(&id.to_string()),
        None,
        None,
        None,
    )
    .await;
    Ok((StatusCode::OK, "链接更新成功"))
}

/// DELETE /api/links/:id — 删除链接
#[tracing::instrument(skip_all)]
pub async fn delete_link(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Path(id): Path<i64>,
) -> AppResult<(StatusCode, &'static str)> {
    let result = sqlx::query("DELETE FROM links WHERE id = ? AND username = ?")
        .bind(id)
        .bind(&session.username)
        .execute(&pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::not_found("记录不存在或无权操作"));
    }

    let _ = log_audit(
        &pool,
        &session.username,
        "delete_link",
        Some(&id.to_string()),
        None,
        None,
        None,
    )
    .await;
    Ok((StatusCode::OK, "链接删除成功"))
}

// ====== HTMX Fragment 处理器 ======
use crate::templates::AppTemplate;
use askama::Template;
use axum::response::IntoResponse;

#[derive(Template)]
#[template(path = "components/link_list.html")]
struct LinkListFragment {
    links: Vec<LinkItem>,
}

#[derive(Template)]
#[template(path = "components/link_form.html")]
struct LinkFormFragment {
    is_edit: bool,
    id: i64,
    title: String,
    url: String,
    icon: String,
}

fn empty_link_form() -> LinkFormFragment {
    LinkFormFragment {
        is_edit: false,
        id: 0,
        title: String::new(),
        url: String::new(),
        icon: String::new(),
    }
}

fn edit_link_form(item: &LinkItem) -> LinkFormFragment {
    LinkFormFragment {
        is_edit: true,
        id: item.id,
        title: item.title.clone(),
        url: item.url.clone(),
        icon: item.icon.clone().unwrap_or_default(),
    }
}

async fn get_user_links(pool: &sqlx::SqlitePool, username: &str) -> Vec<LinkItem> {
    sqlx::query_as::<_, LinkItem>(
        "SELECT id, title, url, icon, sort_order, created_at FROM links WHERE username = ? ORDER BY sort_order, id"
    )
    .bind(username)
    .fetch_all(pool)
    .await
    .unwrap_or_default()
}

#[tracing::instrument(skip_all)]
pub async fn links_list_fragment(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> impl IntoResponse {
    let links = get_user_links(&pool, &session.username).await;
    AppTemplate(LinkListFragment { links })
}

#[tracing::instrument(skip_all)]
pub async fn links_create_fragment(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    axum::extract::Form(form): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let title = form.get("title").cloned().unwrap_or_default();
    let url = form.get("url").cloned().unwrap_or_default();
    if title.trim().is_empty() || url.trim().is_empty() {
        return AppTemplate(empty_link_form()).into_response();
    }
    let icon = form.get("icon").cloned().filter(|s| !s.is_empty());

    let _ = sqlx::query("INSERT INTO links (username, title, url, icon) VALUES (?, ?, ?, ?)")
        .bind(&session.username)
        .bind(&title)
        .bind(&url)
        .bind(&icon)
        .execute(&pool)
        .await;

    let _ = log_audit(
        &pool,
        &session.username,
        "link_create",
        Some(&title),
        None,
        None,
        None,
    )
    .await;

    let links = get_user_links(&pool, &session.username).await;
    AppTemplate(LinkListFragment { links }).into_response()
}

#[tracing::instrument(skip_all)]
pub async fn links_update_fragment(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Path(id): Path<i64>,
    axum::extract::Form(form): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let title = form.get("title").cloned();
    let url = form.get("url").cloned();
    let icon = form.get("icon").cloned().filter(|s| !s.is_empty());

    let _ = sqlx::query(
        "UPDATE links SET title = COALESCE(?, title), url = COALESCE(?, url), icon = COALESCE(?, icon) WHERE id = ? AND username = ?"
    )
    .bind(&title).bind(&url).bind(&icon).bind(id).bind(&session.username)
    .execute(&pool).await;

    let _ = log_audit(
        &pool,
        &session.username,
        "link_update",
        Some(&id.to_string()),
        None,
        None,
        None,
    )
    .await;

    let links = get_user_links(&pool, &session.username).await;
    AppTemplate(LinkListFragment { links })
}

#[tracing::instrument(skip_all)]
pub async fn links_delete_fragment(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let _ = sqlx::query("DELETE FROM links WHERE id = ? AND username = ?")
        .bind(id)
        .bind(&session.username)
        .execute(&pool)
        .await;

    let _ = log_audit(
        &pool,
        &session.username,
        "link_delete",
        Some(&id.to_string()),
        None,
        None,
        None,
    )
    .await;

    let links = get_user_links(&pool, &session.username).await;
    AppTemplate(LinkListFragment { links })
}

#[tracing::instrument(skip_all)]
pub async fn links_empty_form() -> impl IntoResponse {
    AppTemplate(empty_link_form())
}

#[tracing::instrument(skip_all)]
pub async fn links_edit_form(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let item = sqlx::query_as::<_, LinkItem>(
        "SELECT id, title, url, icon, sort_order, created_at FROM links WHERE id = ? AND username = ?"
    )
    .bind(id).bind(&session.username)
    .fetch_optional(&pool).await;

    match item {
        Ok(Some(link)) => AppTemplate(edit_link_form(&link)),
        _ => AppTemplate(empty_link_form()),
    }
}
