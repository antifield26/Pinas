use axum::{
    extract::{Extension, Path, Query},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use pinas_core::UserSession;
use crate::error::{AppError, AppResult};

// ====== DTOs ======

#[derive(Debug, Deserialize, Serialize, FromRow)]
pub struct TodoItem {
    pub id: i64,
    pub username: String,
    pub title: String,
    pub description: Option<String>,
    pub due_date: Option<String>,
    pub is_all_day: i64,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    #[serde(default = "default_priority")]
    pub priority: String,
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default = "default_category")]
    pub category: String,
    pub created_at: String,
    pub updated_at: String,
}

fn default_priority() -> String { "medium".to_string() }
fn default_status() -> String { "pending".to_string() }
fn default_category() -> String { "todo".to_string() }

#[derive(Debug, Deserialize)]
pub struct CreateTodoRequest {
    pub title: String,
    pub description: Option<String>,
    pub due_date: Option<String>,
    #[serde(default = "default_is_all_day")]
    pub is_all_day: bool,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    #[serde(default = "default_priority")]
    pub priority: String,
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default = "default_category")]
    pub category: String,
}

fn default_is_all_day() -> bool { true }

#[derive(Debug, Deserialize)]
pub struct UpdateTodoRequest {
    pub title: Option<String>,
    pub description: Option<String>,
    pub due_date: Option<String>,
    pub is_all_day: Option<bool>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub priority: Option<String>,
    pub status: Option<String>,
    pub category: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TodoQuery {
    pub category: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub search: Option<String>,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
}

// ====== 自动状态计算 ======

/// 根据当前时间计算日程的有效状态。
/// 待办事项(todo)不受影响，直接返回存储状态。
fn compute_effective_status(item: &TodoItem) -> String {
    if item.category != "schedule" {
        return item.status.clone();
    }

    let due_date = match &item.due_date {
        Some(d) if !d.is_empty() => d,
        _ => return "pending".to_string(),
    };

    // 提取日期部分（YYYY-MM-DD），忽略可能的时间部分
    let date_part: String = due_date.chars().take(10).collect();

    let now = chrono::Local::now();
    let today = now.format("%Y-%m-%d").to_string();

    if date_part < today {
        return "expired".to_string();
    }
    if date_part > today {
        return "pending".to_string();
    }

    // 当天：区分全天 / 时间段
    if item.is_all_day != 0 {
        return "in_progress".to_string();
    }

    let current_time = now.format("%H:%M").to_string();
    let start = item.start_time.as_deref().unwrap_or("00:00");
    let end = item.end_time.as_deref().unwrap_or("23:59");

    if current_time.as_str() < start {
        "pending".to_string()
    } else if current_time.as_str() <= end {
        "in_progress".to_string()
    } else {
        "expired".to_string()
    }
}

/// 对日程列表应用自动状态（原地修改 status 字段）
fn apply_auto_status(todos: &mut [TodoItem]) {
    for item in todos.iter_mut() {
        if item.category == "schedule" {
            item.status = compute_effective_status(item);
        }
    }
}

// ====== SQL 列名常量 ======
const TODO_COLS: &str = "id, username, title, description, due_date, is_all_day, start_time, end_time, priority, status, category, created_at, updated_at";

// ====== 处理器 ======

/// GET /api/todos — 获取当前用户的待办/日程列表
pub async fn get_todos(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Query(params): Query<TodoQuery>,
) -> AppResult<Json<Vec<TodoItem>>> {
    let mut sql = String::from(
        "SELECT id, username, title, description, due_date, is_all_day, start_time, end_time, priority, status, category, created_at, updated_at FROM todos WHERE username = ?"
    );
    let mut conditions = Vec::new();
    let mut binds: Vec<String> = vec![session.username.clone()];

    if let Some(ref cat) = params.category {
        conditions.push("category = ?".to_string());
        binds.push(cat.clone());
    }
    // 注意：status 过滤不放在 SQL 中，因为日程的 status 由自动计算决定，
    // 而数据库中始终存储为 "pending"。SQL 预过滤会导致日程被错误排除。
    // 正确的做法是：先查询 → apply_auto_status → 在内存中过滤状态。
    if let Some(ref pr) = params.priority {
        conditions.push("priority = ?".to_string());
        binds.push(pr.clone());
    }
    if let Some(ref s) = params.search {
        conditions.push("(title LIKE ? OR description LIKE ?)".to_string());
        let like = format!("%{}%", s);
        binds.push(like.clone());
        binds.push(like);
    }
    if let Some(ref df) = params.date_from {
        conditions.push("due_date >= ?".to_string());
        binds.push(df.clone());
    }
    if let Some(ref dt) = params.date_to {
        conditions.push("due_date <= ?".to_string());
        binds.push(dt.clone());
    }

    for cond in &conditions {
        sql.push_str(" AND ");
        sql.push_str(cond);
    }
    sql.push_str(" ORDER BY due_date ASC NULLS LAST, priority DESC, created_at DESC");

    let mut query = sqlx::query_as::<_, TodoItem>(&sql);
    for bind in &binds {
        query = query.bind(bind);
    }

    let mut todos = query.fetch_all(&pool).await?;
    // 日程自动计算状态
    apply_auto_status(&mut todos);

    // 如果请求了 status 过滤且过滤的是时间敏感状态，
    // 需要二次过滤（因为日程的 auto-status 可能与存储不同）
    if let Some(ref req_status) = params.status {
        todos.retain(|t| t.status == *req_status);
    }

    Ok(Json(todos))
}

/// POST /api/todos — 创建新待办/日程
pub async fn create_todo(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<CreateTodoRequest>,
) -> AppResult<(StatusCode, Json<TodoItem>)> {
    if payload.title.trim().is_empty() {
        return Err(AppError::bad_request("标题不能为空"));
    }
    if !["low", "medium", "high"].contains(&payload.priority.as_str()) {
        return Err(AppError::bad_request("优先级必须为 low/medium/high"));
    }
    if !["pending", "in_progress", "completed", "expired"].contains(&payload.status.as_str()) {
        return Err(AppError::bad_request("状态必须为 pending/in_progress/completed/expired"));
    }
    if !["todo", "schedule"].contains(&payload.category.as_str()) {
        return Err(AppError::bad_request("类别必须为 todo/schedule"));
    }

    // 日程必须提供截止日期
    let effective_due_date = if payload.category == "schedule" {
        match &payload.due_date {
            Some(d) if !d.trim().is_empty() => Some(d.clone()),
            _ => return Err(AppError::bad_request("日程必须设置日期")),
        }
    } else {
        payload.due_date.clone()
    };

    // 日程状态强制为 pending（由自动计算接管）
    let effective_status = if payload.category == "schedule" {
        "pending".to_string()
    } else {
        payload.status.clone()
    };

    let result = sqlx::query(
        "INSERT INTO todos (username, title, description, due_date, is_all_day, start_time, end_time, priority, status, category) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&session.username)
    .bind(&payload.title)
    .bind(&payload.description)
    .bind(&effective_due_date)
    .bind(payload.is_all_day as i64)
    .bind(&payload.start_time)
    .bind(&payload.end_time)
    .bind(&payload.priority)
    .bind(&effective_status)
    .bind(&payload.category)
    .execute(&pool)
    .await?;

    let id = result.last_insert_rowid();
    let item = sqlx::query_as::<_, TodoItem>(
        &format!("SELECT {} FROM todos WHERE id = ?", TODO_COLS)
    )
    .bind(id)
    .fetch_one(&pool)
    .await?;

    let mut todo = item;
    if todo.category == "schedule" {
        todo.status = compute_effective_status(&todo);
    }
    Ok((StatusCode::CREATED, Json(todo)))
}

/// PUT /api/todos/:id — 更新待办/日程
pub async fn update_todo(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Path(id): Path<i64>,
    Json(payload): Json<UpdateTodoRequest>,
) -> AppResult<Json<TodoItem>> {
    // 校验优先级（若提供）
    if let Some(ref p) = payload.priority {
        if !["low", "medium", "high"].contains(&p.as_str()) {
            return Err(AppError::bad_request("优先级必须为 low/medium/high"));
        }
    }
    if let Some(ref s) = payload.status {
        if !["pending", "in_progress", "completed", "expired"].contains(&s.as_str()) {
            return Err(AppError::bad_request("状态必须为 pending/in_progress/completed/expired"));
        }
    }
    if let Some(ref c) = payload.category {
        if !["todo", "schedule"].contains(&c.as_str()) {
            return Err(AppError::bad_request("类别必须为 todo/schedule"));
        }
    }

    // 先查询现有记录以判断是否为日程
    let existing = sqlx::query_as::<_, TodoItem>(
        &format!("SELECT {} FROM todos WHERE id = ? AND username = ?", TODO_COLS)
    )
    .bind(id)
    .bind(&session.username)
    .fetch_optional(&pool)
    .await?
    .ok_or_else(|| AppError::not_found("记录不存在或无权操作"))?;

    let is_schedule = existing.category == "schedule"
        || payload.category.as_deref() == Some("schedule");

    // 日程不允许手动修改状态
    if is_schedule && payload.status.is_some() {
        return Err(AppError::bad_request("日程状态由系统自动管理，不可手动修改"));
    }

    sqlx::query(
        "UPDATE todos SET title = COALESCE(?, title), description = COALESCE(?, description), due_date = COALESCE(?, due_date), is_all_day = COALESCE(?, is_all_day), start_time = COALESCE(?, start_time), end_time = COALESCE(?, end_time), priority = COALESCE(?, priority), status = COALESCE(?, status), category = COALESCE(?, category), updated_at = datetime('now') WHERE id = ? AND username = ?"
    )
    .bind(&payload.title)
    .bind(&payload.description)
    .bind(&payload.due_date)
    .bind(payload.is_all_day.map(|b| b as i64))
    .bind(&payload.start_time)
    .bind(&payload.end_time)
    .bind(&payload.priority)
    .bind(&payload.status)
    .bind(&payload.category)
    .bind(id)
    .bind(&session.username)
    .execute(&pool)
    .await?;

    let updated = sqlx::query_as::<_, TodoItem>(
        &format!("SELECT {} FROM todos WHERE id = ?", TODO_COLS)
    )
    .bind(id)
    .fetch_optional(&pool)
    .await?
    .ok_or_else(|| AppError::not_found("记录不存在"))?;

    let mut todo = updated;
    if todo.category == "schedule" {
        todo.status = compute_effective_status(&todo);
    }
    Ok(Json(todo))
}

/// DELETE /api/todos/:id — 删除待办/日程
pub async fn delete_todo(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Path(id): Path<i64>,
) -> AppResult<(StatusCode, &'static str)> {
    let result = sqlx::query("DELETE FROM todos WHERE id = ? AND username = ?")
        .bind(id)
        .bind(&session.username)
        .execute(&pool)
        .await?;

    if result.rows_affected() > 0 {
        Ok((StatusCode::OK, "已删除"))
    } else {
        Err(AppError::not_found("记录不存在或无权操作"))
    }
}
