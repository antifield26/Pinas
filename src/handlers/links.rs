#![allow(unused_variables)]
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
            let _ = log_audit(&pool, &session.username, "create_link", Some(&payload.title), None).await;
            (StatusCode::CREATED, "链接添加成功").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("添加失败: {}", e)).into_response(),
    }
}

// 更新链接
pub async fn update_link(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Path(id): Path<i64>,
    Json(payload): Json<UpdateLinkRequest>,
) -> impl IntoResponse {
    let row = sqlx::query("SELECT id FROM links WHERE id = ? AND username = ?")
        .bind(id)
        .bind(&session.username)
        .fetch_optional(&pool)
        .await;
    if row.is_err() || row.unwrap().is_none() {
        return (StatusCode::NOT_FOUND, "链接不存在或无权操作").into_response();
    }

    let mut updates = Vec::new();
    let mut bind_idx = 1;
    if let Some(title) = &payload.title {
        updates.push(format!("title = ?{}", bind_idx));
        bind_idx += 1;
    }
    if let Some(url) = &payload.url {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return (StatusCode::BAD_REQUEST, "URL必须以http://或https://开头").into_response();
        }
        updates.push(format!("url = ?{}", bind_idx));
        bind_idx += 1;
    }
    if let Some(icon) = &payload.icon {
        updates.push(format!("icon = ?{}", bind_idx));
        bind_idx += 1;
    }
    if let Some(order) = payload.sort_order {
        updates.push(format!("sort_order = ?{}", bind_idx));
        bind_idx += 1;
    }
    if updates.is_empty() {
        return (StatusCode::BAD_REQUEST, "没有要更新的字段").into_response();
    }
    let query_str = format!("UPDATE links SET {} WHERE id = ?{}", updates.join(", "), bind_idx);
    let mut query = sqlx::query(&query_str);
    if let Some(title) = &payload.title {
        query = query.bind(title);
    }
    if let Some(url) = &payload.url {
        query = query.bind(url);
    }
    if let Some(icon) = &payload.icon {
        query = query.bind(icon);
    }
    if let Some(order) = payload.sort_order {
        query = query.bind(order);
    }
    query = query.bind(id);
    let result = query.execute(&pool).await;
    match result {
        Ok(_) => {
            let _ = log_audit(&pool, &session.username, "update_link", Some(&id.to_string()), None).await;
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
            let _ = log_audit(&pool, &session.username, "delete_link", Some(&id.to_string()), None).await;
            (StatusCode::OK, "链接删除成功").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("删除失败: {}", e)).into_response(),
    }
}