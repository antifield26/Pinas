use axum::{
    extract::{Extension, Path},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::handlers::utils::log_audit;
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

// 获取当前用户的所有链接
pub async fn get_links(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> impl IntoResponse {
    let rows = sqlx::query_as::<_, LinkItem>(
        "SELECT id, title, url, icon, sort_order, created_at FROM links WHERE username = ? ORDER BY sort_order, id"
    )
    .bind(&session.username)
    .fetch_all(&pool)
    .await;
    match rows {
        Ok(links) => Json(links).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("查询失败: {}", e)).into_response(),
    }
}

// 添加新链接
pub async fn create_link(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<CreateLinkRequest>,
) -> impl IntoResponse {
    if payload.title.trim().is_empty() || payload.url.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "标题和URL不能为空").into_response();
    }
    if !payload.url.starts_with("http://") && !payload.url.starts_with("https://") {
        return (StatusCode::BAD_REQUEST, "URL必须以http://或https://开头").into_response();
    }
    let result = sqlx::query(
        "INSERT INTO links (username, title, url, icon) VALUES (?, ?, ?, ?)"
    )
    .bind(&session.username)
    .bind(&payload.title)
    .bind(&payload.url)
    .bind(&payload.icon)
    .execute(&pool)
    .await;
    match result {
        Ok(_) => {
            let _ = log_audit(&pool, &session.username, "create_link", Some(&payload.title), None, None, None).await;
            (StatusCode::CREATED, "链接添加成功").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("添加失败: {}", e)).into_response(),
    }
}

// 更新链接（使用 COALESCE 避免动态 SQL 拼接）
pub async fn update_link(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Path(id): Path<i64>,
    Json(payload): Json<UpdateLinkRequest>,
) -> impl IntoResponse {
    // URL 格式校验（若提供）
    if let Some(ref url) = payload.url {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return (StatusCode::BAD_REQUEST, "URL必须以http://或https://开头").into_response();
        }
    }
    if payload.title.as_ref().map_or(true, |t| t.trim().is_empty())
        && payload.url.is_none()
        && payload.icon.is_none()
        && payload.sort_order.is_none()
    {
        return (StatusCode::BAD_REQUEST, "没有要更新的字段").into_response();
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
    .await;

    match result {
        Ok(_) => {
            let _ = log_audit(&pool, &session.username, "update_link", Some(&id.to_string()), None, None, None).await;
            (StatusCode::OK, "链接更新成功").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("更新失败: {}", e)).into_response(),
    }
}

// 删除链接
pub async fn delete_link(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let result = sqlx::query("DELETE FROM links WHERE id = ? AND username = ?")
        .bind(id)
        .bind(&session.username)
        .execute(&pool)
        .await;
    match result {
        Ok(_) => {
            let _ = log_audit(&pool, &session.username, "delete_link", Some(&id.to_string()), None, None, None).await;
            (StatusCode::OK, "链接删除成功").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("删除失败: {}", e)).into_response(),
    }
}