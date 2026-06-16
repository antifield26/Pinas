use axum::{
    extract::{Extension, Path},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::handlers::utils::log_audit;
use crate::error::{AppError, AppResult};
use pinas_core::UserSession;

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
    let _ = log_audit(&pool, &session.username, "create_link", Some(&payload.title), None, None, None).await;
    Ok((StatusCode::CREATED, "链接添加成功"))
}

/// PUT /api/links/:id — 更新链接（使用 COALESCE 避免动态 SQL 拼接）
pub async fn update_link(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Path(id): Path<i64>,
    Json(payload): Json<UpdateLinkRequest>,
) -> AppResult<(StatusCode, &'static str)> {
    if let Some(ref url) = payload.url {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(AppError::bad_request("URL必须以http://或https://开头"));
        }
    }
    if payload.title.as_ref().map_or(true, |t| t.trim().is_empty())
        && payload.url.is_none()
        && payload.icon.is_none()
        && payload.sort_order.is_none()
    {
        return Err(AppError::bad_request("没有要更新的字段"));
    }

    sqlx::query(
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

    let _ = log_audit(&pool, &session.username, "update_link", Some(&id.to_string()), None, None, None).await;
    Ok((StatusCode::OK, "链接更新成功"))
}

/// DELETE /api/links/:id — 删除链接
pub async fn delete_link(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Path(id): Path<i64>,
) -> AppResult<(StatusCode, &'static str)> {
    sqlx::query("DELETE FROM links WHERE id = ? AND username = ?")
        .bind(id)
        .bind(&session.username)
        .execute(&pool)
        .await?;

    let _ = log_audit(&pool, &session.username, "delete_link", Some(&id.to_string()), None, None, None).await;
    Ok((StatusCode::OK, "链接删除成功"))
}
