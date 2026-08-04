// ====== AI 对话管理 ======
use axum::{
    extract::{Extension, Path, Query},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::core::UserSession;
use crate::error::AppResult;
use crate::templates::AppTemplate;
use askama::Template;

#[derive(Serialize, sqlx::FromRow)]
pub struct Conversation {
    pub id: i64,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Template)]
#[template(path = "components/conversation_list.html")]
struct ConversationListFragment {
    conversations: Vec<Conversation>,
    active_id: i64,
}

/// GET /api/conversations — 列出对话
pub async fn list_conversations(
    Extension(pool): Extension<SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> AppResult<Json<Vec<Conversation>>> {
    let rows = sqlx::query_as::<_, Conversation>(
        "SELECT id, title, created_at, updated_at FROM conversations WHERE username = ? ORDER BY updated_at DESC"
    ).bind(&session.username).fetch_all(&pool).await?;
    Ok(Json(rows))
}

/// POST /api/conversations — 新建对话
pub async fn create_conversation(
    Extension(pool): Extension<SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> AppResult<(StatusCode, Json<Conversation>)> {
    let id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO conversations (username, title) VALUES (?, '新对话') RETURNING id",
    )
    .bind(&session.username)
    .fetch_one(&pool)
    .await?;
    let conv = sqlx::query_as::<_, Conversation>(
        "SELECT id, title, created_at, updated_at FROM conversations WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&pool)
    .await?;
    Ok((StatusCode::CREATED, Json(conv)))
}

/// PUT /api/conversations/:id — 重命名对话
pub async fn rename_conversation(
    Extension(pool): Extension<SqlitePool>,
    Extension(session): Extension<UserSession>,
    Path(id): Path<i64>,
    Json(body): Json<ConvRenameRequest>,
) -> AppResult<Json<Conversation>> {
    sqlx::query("UPDATE conversations SET title = ?, updated_at = datetime('now') WHERE id = ? AND username = ?")
        .bind(&body.title).bind(id).bind(&session.username).execute(&pool).await?;
    let conv = sqlx::query_as::<_, Conversation>(
        "SELECT id, title, created_at, updated_at FROM conversations WHERE id = ?",
    )
    .bind(id)
    .fetch_one(&pool)
    .await?;
    Ok(Json(conv))
}

/// DELETE /api/conversations/:id — 删除对话
pub async fn delete_conversation(
    Extension(pool): Extension<SqlitePool>,
    Extension(session): Extension<UserSession>,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    sqlx::query("DELETE FROM conversations WHERE id = ? AND username = ?")
        .bind(id)
        .bind(&session.username)
        .execute(&pool)
        .await?;
    Ok(StatusCode::OK)
}

/// GET /agent/conversations — HTMX 对话列表片段
pub async fn conversation_list_fragment(
    Extension(pool): Extension<SqlitePool>,
    Extension(session): Extension<UserSession>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let active_id: i64 = params
        .get("active")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let rows = sqlx::query_as::<_, Conversation>(
        "SELECT id, title, created_at, updated_at FROM conversations WHERE username = ? ORDER BY updated_at DESC"
    ).bind(&session.username).fetch_all(&pool).await.unwrap_or_default();
    AppTemplate(ConversationListFragment {
        conversations: rows,
        active_id,
    })
}

#[derive(Deserialize)]
pub struct ConvRenameRequest {
    pub title: String,
}
