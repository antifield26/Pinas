use axum::{
    extract::{Extension, Path, Query},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::core::UserSession;
use crate::error::{AppError, AppResult};
use crate::handlers::utils::log_audit;

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

fn default_priority() -> String {
    "medium".to_string()
}
fn default_status() -> String {
    "pending".to_string()
}
fn default_category() -> String {
    "todo".to_string()
}

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

fn default_is_all_day() -> bool {
    true
}

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
    pub date: Option<String>, // exact date match (for calendar drill-down)
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
#[tracing::instrument(skip_all)]
pub async fn get_todos(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Query(params): Query<TodoQuery>,
) -> AppResult<Json<Vec<TodoItem>>> {
    let mut builder = sqlx::QueryBuilder::new(
        "SELECT id, username, title, description, due_date, is_all_day, start_time, end_time, priority, status, category, created_at, updated_at FROM todos WHERE username = ",
    );
    builder.push_bind(session.username.clone());

    // 注意：status 过滤不放在 SQL 中，因为日程的 status 由自动计算决定
    if let Some(ref cat) = params.category {
        builder.push(" AND category = ");
        builder.push_bind(cat.clone());
    }
    if let Some(ref pr) = params.priority {
        builder.push(" AND priority = ");
        builder.push_bind(pr.clone());
    }
    if let Some(ref s) = params.search {
        builder.push(" AND (title LIKE ");
        builder.push_bind(format!("%{}%", s));
        builder.push(" OR description LIKE ");
        builder.push_bind(format!("%{}%", s));
        builder.push(")");
    }
    // 带时间的日程 due_date 形如 "2026-08-13T10:00:00"，与纯日期字符串
    // 直接比较时 'T'(0x54) > '3'，导致带时间日程被日期筛选全部漏掉——统一按前 10 字符比较
    if let Some(ref df) = params.date_from {
        builder.push(" AND substr(due_date, 1, 10) >= ");
        builder.push_bind(df.clone());
    }
    if let Some(ref dt) = params.date_to {
        builder.push(" AND substr(due_date, 1, 10) <= ");
        builder.push_bind(dt.clone());
    }
    if let Some(ref d) = params.date {
        builder.push(" AND substr(due_date, 1, 10) = ");
        builder.push_bind(d.clone());
    }

    builder.push(" ORDER BY due_date ASC NULLS LAST, priority DESC, created_at DESC");

    let query = builder.build_query_as::<TodoItem>();

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
#[tracing::instrument(skip_all)]
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
        return Err(AppError::bad_request(
            "状态必须为 pending/in_progress/completed/expired",
        ));
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
    let sql = format!("SELECT {} FROM todos WHERE id = ?", TODO_COLS);
    let item = sqlx::query_as::<_, TodoItem>(sqlx::AssertSqlSafe(sql.as_str()))
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
#[tracing::instrument(skip_all)]
pub async fn update_todo(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Path(id): Path<i64>,
    Json(payload): Json<UpdateTodoRequest>,
) -> AppResult<Json<TodoItem>> {
    // 校验优先级（若提供）
    if let Some(ref p) = payload.priority
        && !["low", "medium", "high"].contains(&p.as_str())
    {
        return Err(AppError::bad_request("优先级必须为 low/medium/high"));
    }
    if let Some(ref s) = payload.status
        && !["pending", "in_progress", "completed", "expired"].contains(&s.as_str())
    {
        return Err(AppError::bad_request(
            "状态必须为 pending/in_progress/completed/expired",
        ));
    }
    if let Some(ref c) = payload.category
        && !["todo", "schedule"].contains(&c.as_str())
    {
        return Err(AppError::bad_request("类别必须为 todo/schedule"));
    }

    // 先查询现有记录以判断是否为日程
    let sql = format!(
        "SELECT {} FROM todos WHERE id = ? AND username = ?",
        TODO_COLS
    );
    let existing = sqlx::query_as::<_, TodoItem>(sqlx::AssertSqlSafe(sql.as_str()))
        .bind(id)
        .bind(&session.username)
        .fetch_optional(&pool)
        .await?
        .ok_or_else(|| AppError::not_found("记录不存在或无权操作"))?;

    let is_schedule =
        existing.category == "schedule" || payload.category.as_deref() == Some("schedule");

    // 日程不允许手动修改状态
    if is_schedule && payload.status.is_some() {
        return Err(AppError::bad_request(
            "日程状态由系统自动管理，不可手动修改",
        ));
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

    let sql = format!("SELECT {} FROM todos WHERE id = ?", TODO_COLS);
    let updated = sqlx::query_as::<_, TodoItem>(sqlx::AssertSqlSafe(sql.as_str()))
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
#[tracing::instrument(skip_all)]
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

// ====== HTMX Fragment 处理器 ======
use crate::templates::AppTemplate;
use askama::Template;
use axum::response::IntoResponse;

/// Fragment: 待办列表（含计数徽章）
#[derive(Template)]
#[template(path = "components/todo_list.html")]
struct TodoListFragment {
    todos: Vec<TodoItem>,
}

/// Fragment: 待办表单（新建/编辑）
#[derive(Template)]
#[template(path = "components/todo_form.html")]
struct TodoFormFragment {
    is_edit: bool,
    id: i64,
    category: String,
    title: String,
    description: String,
    due_date: String,
    is_all_day: String,
    start_time: String,
    end_time: String,
    todo_due_date: String,
    todo_due_time: String,
    priority: String,
    status: String,
    show_date: bool,
}

/// 构建空表单
fn empty_form() -> TodoFormFragment {
    TodoFormFragment {
        is_edit: false,
        id: 0,
        category: "todo".into(),
        title: String::new(),
        description: String::new(),
        due_date: String::new(),
        is_all_day: "true".into(),
        start_time: String::new(),
        end_time: String::new(),
        todo_due_date: String::new(),
        todo_due_time: String::new(),
        priority: "medium".into(),
        status: "pending".into(),
        show_date: false,
    }
}

/// 从 TodoItem 构建编辑表单
fn edit_form(item: &TodoItem) -> TodoFormFragment {
    let (todo_due_date, todo_due_time) = if item.category == "todo" {
        if let Some(ref d) = item.due_date {
            if d.len() > 10 {
                (d[..10].to_string(), d[11..16].to_string())
            } else {
                (d.clone(), String::new())
            }
        } else {
            (String::new(), String::new())
        }
    } else {
        (String::new(), String::new())
    };

    let due_date = item.due_date.as_deref().unwrap_or("").to_string();
    let is_all_day = if item.is_all_day != 0 {
        "true"
    } else {
        "false"
    };

    TodoFormFragment {
        is_edit: true,
        id: item.id,
        category: item.category.clone(),
        title: item.title.clone(),
        description: item.description.clone().unwrap_or_default(),
        due_date,
        is_all_day: is_all_day.to_string(),
        start_time: item.start_time.clone().unwrap_or_default(),
        end_time: item.end_time.clone().unwrap_or_default(),
        todo_due_date,
        todo_due_time,
        priority: item.priority.clone(),
        status: item.status.clone(),
        show_date: item.due_date.is_some(),
    }
}

/// 获取待办列表（复用查询逻辑）
async fn get_todos_filtered(
    pool: &sqlx::SqlitePool,
    username: &str,
    category: Option<String>,
    status: Option<String>,
) -> Result<Vec<TodoItem>, AppError> {
    let mut builder = sqlx::QueryBuilder::new(
        "SELECT id, username, title, description, due_date, is_all_day, start_time, end_time, priority, status, category, created_at, updated_at FROM todos WHERE username = ",
    );
    builder.push_bind(username);

    if let Some(ref cat) = category
        && cat != "all"
    {
        builder.push(" AND category = ");
        builder.push_bind(cat.clone());
    }
    builder.push(" ORDER BY CASE WHEN category = 'schedule' THEN 0 ELSE 1 END, due_date ASC NULLS LAST, CASE WHEN priority = 'high' THEN 0 WHEN priority = 'medium' THEN 1 ELSE 2 END");

    let query = builder.build_query_as::<TodoItem>();
    let mut todos = query.fetch_all(pool).await?;
    apply_auto_status(&mut todos);

    if let Some(ref s) = status
        && s != "all"
    {
        todos.retain(|t| t.status == *s);
    }

    Ok(todos)
}

/// GET /todos/list — 待办列表 HTML 片段
#[tracing::instrument(skip_all)]
pub async fn todos_list_fragment(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    axum::extract::Query(params): axum::extract::Query<TodoQuery>,
) -> impl axum::response::IntoResponse {
    let category = params.category.unwrap_or_else(|| "all".to_string());
    let status = params.status.unwrap_or_else(|| "all".to_string());

    match get_todos_filtered(
        &pool,
        &session.username,
        Some(category.clone()),
        Some(status.clone()),
    )
    .await
    {
        Ok(todos) => AppTemplate(TodoListFragment { todos }),
        Err(_) => AppTemplate(TodoListFragment { todos: vec![] }),
    }
}

/// POST /todos — 创建待办（HTMX 表单提交）
#[tracing::instrument(skip_all)]
pub async fn todos_create_fragment(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    axum::extract::Form(form): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> impl axum::response::IntoResponse {
    let title = form.get("title").cloned().unwrap_or_default();
    if title.trim().is_empty() {
        return AppTemplate(empty_form()).into_response();
    }

    let category = form
        .get("category")
        .cloned()
        .unwrap_or_else(|| "todo".to_string());
    let description = form.get("description").cloned().filter(|s| !s.is_empty());
    let priority = form
        .get("priority")
        .cloned()
        .unwrap_or_else(|| "medium".to_string());
    let is_all_day = form.get("is_all_day").map(|v| v == "true").unwrap_or(true);

    let (due_date, start_time, end_time) = if category == "schedule" {
        let date = form.get("due_date").cloned().unwrap_or_default();
        if date.is_empty() {
            return AppTemplate(empty_form()).into_response();
        }
        let (st, et) = if !is_all_day {
            (
                form.get("start_time").cloned(),
                form.get("end_time").cloned(),
            )
        } else {
            (None, None)
        };
        (Some(date), st, et)
    } else {
        let date = form.get("todo_due_date").cloned().filter(|d| !d.is_empty());
        let time = form.get("todo_due_time").cloned().filter(|t| !t.is_empty());
        let due = match (date, time) {
            (Some(d), Some(t)) => Some(format!("{}T{}:00", d, t)),
            (Some(d), None) => Some(d),
            _ => None,
        };
        (due, None, None)
    };

    let _ = sqlx::query(
        "INSERT INTO todos (username, title, description, due_date, is_all_day, start_time, end_time, priority, status, category) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&session.username).bind(&title).bind(&description).bind(&due_date)
    .bind(is_all_day as i64).bind(&start_time).bind(&end_time)
    .bind(&priority).bind("pending").bind(&category)
    .execute(&pool).await;

    let _ = log_audit(
        &pool,
        &session.username,
        "todo_create",
        Some(&title),
        None,
        None,
        None,
    )
    .await;

    // 返回更新后的列表
    get_todos_filtered(&pool, &session.username, None, None::<String>)
        .await
        .map(|todos| AppTemplate(TodoListFragment { todos }).into_response())
        .unwrap_or_else(|_| AppTemplate(TodoListFragment { todos: vec![] }).into_response())
}

/// PUT /todos/:id — 更新待办（HTMX 表单提交）
#[tracing::instrument(skip_all)]
pub async fn todos_update_fragment(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Path(id): Path<i64>,
    axum::extract::Form(form): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> impl axum::response::IntoResponse {
    // 先获取现有记录
    let fetch_sql = format!(
        "SELECT {} FROM todos WHERE id = ? AND username = ?",
        TODO_COLS
    );
    let existing = sqlx::query_as::<_, TodoItem>(sqlx::AssertSqlSafe(fetch_sql.as_str()))
        .bind(id)
        .bind(&session.username)
        .fetch_optional(&pool)
        .await;

    let existing = match existing {
        Ok(Some(item)) => item,
        _ => return (AppTemplate(TodoListFragment { todos: vec![] })).into_response(),
    };

    let title = form.get("title").cloned();
    let description = form.get("description").cloned().filter(|s| !s.is_empty());
    let priority = form.get("priority").cloned();
    let status = form.get("status").cloned();
    let is_all_day = form.get("is_all_day").map(|v| v == "true");

    let (due_date, start_time, end_time, is_all_day_val) = if existing.category == "schedule" {
        let d = form.get("due_date").cloned().or(existing.due_date.clone());
        let ia = is_all_day.unwrap_or(existing.is_all_day != 0);
        let (st, et) = if !ia {
            (
                form.get("start_time")
                    .cloned()
                    .or(existing.start_time.clone()),
                form.get("end_time").cloned().or(existing.end_time.clone()),
            )
        } else {
            (None, None)
        };
        (d, st, et, ia)
    } else {
        let date = form.get("todo_due_date").cloned().filter(|d| !d.is_empty());
        let time = form.get("todo_due_time").cloned().filter(|t| !t.is_empty());
        let due = match (date, time) {
            (Some(d), Some(t)) => Some(format!("{}T{}:00", d, t)),
            (Some(d), None) => Some(d),
            _ => existing.due_date.clone(),
        };
        (due, None, None, existing.is_all_day != 0)
    };

    let effective_status = if existing.category == "schedule" {
        None
    } else {
        status
    };

    let _ = sqlx::query(
        "UPDATE todos SET title = COALESCE(?, title), description = COALESCE(?, description), due_date = COALESCE(?, due_date), is_all_day = COALESCE(?, is_all_day), start_time = COALESCE(?, start_time), end_time = COALESCE(?, end_time), priority = COALESCE(?, priority), status = COALESCE(?, status), updated_at = datetime('now') WHERE id = ? AND username = ?"
    )
    .bind(&title).bind(&description).bind(&due_date)
    .bind(Some(is_all_day_val as i64)).bind(&start_time).bind(&end_time)
    .bind(&priority).bind(&effective_status)
    .bind(id).bind(&session.username)
    .execute(&pool).await;

    let _ = log_audit(
        &pool,
        &session.username,
        "todo_update",
        Some(&id.to_string()),
        None,
        None,
        None,
    )
    .await;

    // 返回更新后的列表
    get_todos_filtered(&pool, &session.username, None, None::<String>)
        .await
        .map(|todos| AppTemplate(TodoListFragment { todos }).into_response())
        .unwrap_or_else(|_| AppTemplate(TodoListFragment { todos: vec![] }).into_response())
}

/// DELETE /todos/:id — 删除待办（HTMX）
#[tracing::instrument(skip_all)]
pub async fn todos_delete_fragment(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Path(id): Path<i64>,
) -> impl axum::response::IntoResponse {
    let _ = sqlx::query("DELETE FROM todos WHERE id = ? AND username = ?")
        .bind(id)
        .bind(&session.username)
        .execute(&pool)
        .await;

    let _ = log_audit(
        &pool,
        &session.username,
        "todo_delete",
        Some(&id.to_string()),
        None,
        None,
        None,
    )
    .await;

    match get_todos_filtered(&pool, &session.username, None, None::<String>).await {
        Ok(todos) => AppTemplate(TodoListFragment { todos }),
        Err(_) => AppTemplate(TodoListFragment { todos: vec![] }),
    }
}

/// GET /todos/form — 空表单
#[tracing::instrument(skip_all)]
pub async fn todos_empty_form() -> impl axum::response::IntoResponse {
    AppTemplate(empty_form())
}

/// Calendar cell data
struct CalendarCell {
    day: i32,
    date: String,
    count: usize,
}

/// Calendar fragment template
#[derive(Template)]
#[template(path = "components/todo_calendar.html")]
struct TodoCalendarFragment {
    year: i32,
    month: i32,
    prev_year: i32,
    prev_month: i32,
    next_year: i32,
    next_month: i32,
    cells: Vec<CalendarCell>,
}

/// GET /todos/calendar?year=&month= — 日历视图 fragment
#[tracing::instrument(skip_all)]
pub async fn todos_calendar_fragment(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Query(params): Query<TodoCalendarQuery>,
) -> impl IntoResponse {
    use chrono::Datelike;
    // Local 时区：与 compute_effective_status 一致（UTC 下 UTC+8 用户"今天"会偏 8 小时）
    let now = chrono::Local::now();
    let year = params.year.unwrap_or(now.year());
    let month = params.month.unwrap_or(now.month() as i32).clamp(1, 12);

    let (prev_year, prev_month) = if month == 1 {
        (year - 1, 12)
    } else {
        (year, month - 1)
    };
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };

    // 计算当月天数
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 30,
    };

    // 计算当月第一天是周几 (0=Sun)
    let first_weekday = {
        let d = chrono::NaiveDate::from_ymd_opt(year, month as u32, 1)
            .unwrap_or(chrono::NaiveDate::from_ymd_opt(2024, 1, 1).unwrap());
        d.weekday().num_days_from_sunday() as i32
    };

    // 查询当月所有 todos
    let month_start = format!("{}-{:02}-01", year, month);
    let month_end = format!("{}-{:02}-{:02}", year, month, days_in_month);
    // 带时间的日程（"2026-08-13T10:00:00"）与 "2026-08-31" 字符串比较会因 'T' > '3' 被漏掉，
    // 比较与统计键均取日期部分（前 10 字符）
    let sql = format!(
        "SELECT {} FROM todos WHERE username = ? AND substr(due_date, 1, 10) >= ? AND substr(due_date, 1, 10) <= ? ORDER BY due_date",
        TODO_COLS
    );
    let todos: Vec<TodoItem> = sqlx::query_as(sqlx::AssertSqlSafe(sql.as_str()))
        .bind(&session.username)
        .bind(&month_start)
        .bind(&month_end)
        .fetch_all(&pool)
        .await
        .unwrap_or_default();

    // 按日期统计（键取日期部分，与日历格子键一致）
    use std::collections::HashMap;
    let mut date_counts: HashMap<String, usize> = HashMap::new();
    for t in &todos {
        if let Some(ref d) = t.due_date {
            let key: String = d.chars().take(10).collect();
            *date_counts.entry(key).or_insert(0) += 1;
        }
    }

    // 构建日历格子
    let mut cells = Vec::with_capacity(42);
    // 填充上月空白
    for _ in 0..first_weekday {
        cells.push(CalendarCell {
            day: 0,
            date: String::new(),
            count: 0,
        });
    }
    // 填充当月日期
    for day in 1..=days_in_month {
        let date = format!("{}-{:02}-{:02}", year, month, day);
        let count = date_counts.get(&date).copied().unwrap_or(0);
        cells.push(CalendarCell { day, date, count });
    }

    AppTemplate(TodoCalendarFragment {
        year,
        month,
        prev_year,
        prev_month,
        next_year,
        next_month,
        cells,
    })
}

/// Calendar query params
#[derive(Deserialize)]
pub struct TodoCalendarQuery {
    pub year: Option<i32>,
    pub month: Option<i32>,
}

/// GET /todos/form/:id — 编辑表单
#[tracing::instrument(skip_all)]
pub async fn todos_edit_form(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Path(id): Path<i64>,
) -> impl axum::response::IntoResponse {
    let fetch_sql = format!(
        "SELECT {} FROM todos WHERE id = ? AND username = ?",
        TODO_COLS
    );
    let item = sqlx::query_as::<_, TodoItem>(sqlx::AssertSqlSafe(fetch_sql.as_str()))
        .bind(id)
        .bind(&session.username)
        .fetch_optional(&pool)
        .await;

    match item {
        Ok(Some(mut todo)) => {
            if todo.category == "schedule" {
                todo.status = compute_effective_status(&todo);
            }
            AppTemplate(edit_form(&todo))
        }
        _ => AppTemplate(empty_form()),
    }
}
