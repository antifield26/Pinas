use axum::{
    extract::Extension,
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::sync::LazyLock;

use crate::config::Config;
use crate::core::UserSession;

/// 用户 Agent 设置行（user_settings 表一行）：(api_key, api_base, model, temperature, max_tokens)
type UserSettingsRow = Option<(
    Option<String>,
    Option<String>,
    Option<String>,
    Option<f32>,
    Option<u32>,
)>;

// ====== 共享 HTTP 客户端 ======
static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("Failed to create HTTP client")
});

// ====== 系统提示缓存 (30s TTL) ======
use tokio::sync::Mutex;
static PROMPT_CACHE: LazyLock<
    Mutex<std::collections::HashMap<String, (String, std::time::Instant)>>,
> = LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

// ====== 模型列表 ======
const AVAILABLE_MODELS: &[(&str, &str)] = &[
    ("deepseek-v4-pro", "DeepSeek V4 Pro (推荐)"),
    ("deepseek-v4-flash", "DeepSeek V4 Flash (快速)"),
];

// ====== DTOs ======

#[derive(Debug, Deserialize)]
pub struct ChatMessage {
    pub role: String, // "user" | "assistant" | "system"
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// 会话 ID：提供时消息持久化到该对话并从库中加载上下文
    #[serde(default)]
    pub conversation_id: Option<i64>,
}

fn default_temperature() -> f32 {
    0.7
}
fn default_max_tokens() -> u32 {
    4096
}

#[derive(Debug, Serialize)]
pub struct ChatResponse {
    pub reply: String,
    pub model: String,
    pub usage: Option<UsageInfo>,
    /// 实际写入的会话 ID（首次对话自动创建后回传，前端用于后续轮次）
    pub conversation_id: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UsageInfo {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

// DeepSeek API 请求/响应格式（OpenAI 兼容）
#[derive(Debug, Serialize)]
struct DeepSeekRequest {
    model: String,
    messages: Vec<DeepSeekMessage>,
    temperature: f32,
    max_tokens: u32,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct DeepSeekMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct DeepSeekResponse {
    choices: Vec<DeepSeekChoice>,
    usage: Option<DeepSeekUsage>,
}

#[derive(Debug, Deserialize)]
struct DeepSeekChoice {
    message: DeepSeekMessageResp,
}

#[derive(Debug, Deserialize)]
struct DeepSeekMessageResp {
    content: String,
}

#[derive(Debug, Deserialize)]
struct DeepSeekUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

// ====== AI 系统提示词 ======
const SYSTEM_PROMPT_BASE: &str = r#"你是 Antifield Cloud 的 AI 智能助手。你可以帮助用户：

1. **任务管理** — 帮助整理待办事项，建议优先级排序
2. **日程规划** — 根据日程生成合理的每日计划
3. **每日简报** — 汇总用户的待办和日程，生成清晰的每日简报
4. **文件建议** — 提供文件组织和管理建议
5. **通用问答** — 回答各类问题

请用中文回复，风格简洁专业、友好亲切。回复使用 Markdown 格式。"#;

/// 带缓存的系统提示词获取（30s TTL，减少每次请求的 DB 查询延迟）
async fn get_cached_prompt(pool: &SqlitePool, username: &str) -> String {
    let mut cache = PROMPT_CACHE.lock().await;
    let now = std::time::Instant::now();
    if let Some((cached, ts)) = cache.get(username)
        && now.duration_since(*ts) < std::time::Duration::from_secs(30)
    {
        return cached.clone();
    }
    let prompt = build_system_prompt(pool, username).await;
    cache.insert(username.to_string(), (prompt.clone(), now));
    // 防止缓存无限增长
    if cache.len() > 100 {
        cache.clear();
    }
    prompt
}

/// 构建包含用户待办/日程上下文的动态系统提示词
async fn build_system_prompt(pool: &SqlitePool, username: &str) -> String {
    let mut prompt = SYSTEM_PROMPT_BASE.to_string();

    // 查询用户未完成的待办和今日/未来日程
    let items: Vec<TodoContextItem> = sqlx::query_as(
        "SELECT title, description, due_date, priority, status, category, is_all_day, start_time, end_time \
         FROM todos WHERE username = ? \
         AND ((category = 'todo' AND status != 'completed') \
              OR (category = 'schedule' AND due_date >= date('now'))) \
         ORDER BY CASE WHEN category = 'schedule' THEN 0 ELSE 1 END, \
                  due_date ASC NULLS LAST, \
                  CASE WHEN priority = 'high' THEN 0 WHEN priority = 'medium' THEN 1 ELSE 2 END"
    )
    .bind(username)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    if items.is_empty() {
        return prompt;
    }

    let todos: Vec<_> = items
        .iter()
        .filter(|i| i.category.as_deref() == Some("todo"))
        .collect();
    let schedules: Vec<_> = items
        .iter()
        .filter(|i| i.category.as_deref() == Some("schedule"))
        .collect();

    prompt.push_str("\n\n---\n## 当前用户数据\n\n");

    if !todos.is_empty() {
        prompt.push_str("### 待办事项\n\n");
        for t in &todos {
            let priority_icon = match t.priority.as_deref() {
                Some("high") => "🔴",
                Some("medium") => "🟡",
                _ => "🟢",
            };
            let status_label = match t.status.as_deref() {
                Some("in_progress") => "[进行中]",
                _ => "[待办]",
            };
            prompt.push_str(&format!(
                "- {} {} **{}**",
                priority_icon,
                status_label,
                t.title.as_deref().unwrap_or("无标题")
            ));
            if let Some(ref desc) = t.description
                && !desc.trim().is_empty()
            {
                prompt.push_str(&format!(" — {}", desc));
            }
            if let Some(ref due) = t.due_date {
                prompt.push_str(&format!(" (截止: {})", due));
            }
            prompt.push('\n');
        }
        prompt.push('\n');
    }

    if !schedules.is_empty() {
        prompt.push_str("### 日程安排\n\n");
        for s in &schedules {
            let date_str = s.due_date.as_deref().unwrap_or("未设置日期");
            let time_str = if s.is_all_day != Some(1) {
                match (s.start_time.as_deref(), s.end_time.as_deref()) {
                    (Some(st), Some(et)) => format!(" {}–{}", st, et),
                    _ => String::new(),
                }
            } else {
                " 全天".to_string()
            };
            let date_time_label = format!("{}{}", date_str, time_str);
            prompt.push_str(&format!(
                "- **{}** ({})",
                s.title.as_deref().unwrap_or("无标题"),
                date_time_label
            ));
            if let Some(ref desc) = s.description
                && !desc.trim().is_empty()
            {
                prompt.push_str(&format!(" — {}", desc));
            }
            prompt.push('\n');
        }
        prompt.push('\n');
    }

    prompt.push_str("请基于以上真实数据回答用户的问题。如果用户询问待办/日程相关问题，务必参考以上数据如实回答，不要编造不存在的事项。\n");
    prompt
}

/// 用于查询 todos 上下文的精简结构体
#[derive(Debug, sqlx::FromRow)]
struct TodoContextItem {
    title: Option<String>,
    description: Option<String>,
    due_date: Option<String>,
    priority: Option<String>,
    status: Option<String>,
    category: Option<String>,
    is_all_day: Option<i64>,
    start_time: Option<String>,
    end_time: Option<String>,
}

// ====== 处理器 ======

/// 解析用户 Agent 配置：用户设置 > 全局配置
struct ResolvedAgentConfig {
    api_key: String,
    api_base: String,
    model: String,
    temperature: f32,
    max_tokens: u32,
}

async fn resolve_agent_config(
    pool: &SqlitePool,
    config: &Config,
    session: &UserSession,
    requested_model: Option<String>,
) -> Result<ResolvedAgentConfig, (StatusCode, String)> {
    // 尝试读取用户设置
    let user_settings: UserSettingsRow = sqlx::query_as(
        "SELECT deepseek_api_key, deepseek_api_base, deepseek_model, temperature, max_tokens FROM user_settings WHERE username = ?"
    )
    .bind(&session.username)
    .fetch_optional(pool)
    .await
    .unwrap_or(None);

    let user_api_key = user_settings.as_ref().and_then(|u| u.0.clone());
    let user_api_base = user_settings.as_ref().and_then(|u| u.1.clone());
    let user_model = user_settings.as_ref().and_then(|u| u.2.clone());
    let user_temperature = user_settings.as_ref().and_then(|u| u.3).unwrap_or(0.7);
    let user_max_tokens = user_settings.as_ref().and_then(|u| u.4).unwrap_or(4096);

    // API Key: 用户设置 > 全局配置
    let user_has_key = user_api_key
        .as_deref()
        .is_some_and(|k| !k.trim().is_empty());
    let api_key = match user_api_key.or_else(|| config.deepseek_api_key.clone()) {
        Some(k) if !k.is_empty() => k,
        _ => return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "AI 服务未配置。请在 Agent 设置中配置 API Key 或设置环境变量 PINAS_DEEPSEEK_API_KEY"
                .to_string(),
        )),
    };

    // API Base: 用户自配 key 时才允许自配 base；
    // 使用全局 key 时强制全局 base（防止把全局 key 发往用户可控服务器造成凭据窃取）
    let api_base = if user_has_key {
        user_api_base
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| config.deepseek_api_base.clone())
    } else {
        config.deepseek_api_base.clone()
    };

    // Model: 请求参数 > 用户设置 > 全局配置
    let model = requested_model
        .filter(|m| !m.is_empty())
        .or_else(|| user_model.filter(|m| !m.is_empty()))
        .unwrap_or_else(|| config.deepseek_model.clone());

    Ok(ResolvedAgentConfig {
        api_key,
        api_base,
        model,
        temperature: user_temperature,
        max_tokens: user_max_tokens,
    })
}

/// GET /api/agent/models — 返回可用模型列表
pub async fn get_models() -> impl IntoResponse {
    let models: Vec<serde_json::Value> = AVAILABLE_MODELS
        .iter()
        .map(|(id, name)| serde_json::json!({ "id": id, "name": name }))
        .collect();
    Json(models)
}

// ====== 对话消息持久化 ======

/// 保存一轮对话消息；无 conversation_id 时自动创建对话（标题取首条用户消息截断）
async fn save_chat_round(
    pool: &SqlitePool,
    username: &str,
    conversation_id: Option<i64>,
    user_msg: &str,
    assistant_reply: &str,
) -> Result<i64, sqlx::Error> {
    let conv_id = match conversation_id {
        Some(id) => id,
        None => {
            let title: String = user_msg.chars().take(20).collect();
            let title = if title.len() < user_msg.len() {
                format!("{}…", title)
            } else {
                title
            };
            sqlx::query_scalar(
                "INSERT INTO conversations (username, title) VALUES (?, ?) RETURNING id",
            )
            .bind(username)
            .bind(title)
            .fetch_one(pool)
            .await?
        }
    };
    sqlx::query(
        "INSERT INTO conversation_messages (conversation_id, role, content) VALUES (?, 'user', ?)",
    )
    .bind(conv_id)
    .bind(user_msg)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO conversation_messages (conversation_id, role, content) VALUES (?, 'assistant', ?)",
    )
    .bind(conv_id)
    .bind(assistant_reply)
    .execute(pool)
    .await?;
    sqlx::query("UPDATE conversations SET updated_at = datetime('now') WHERE id = ?")
        .bind(conv_id)
        .execute(pool)
        .await?;
    Ok(conv_id)
}

/// 加载对话历史（最近 limit 条 user/assistant 消息，按时间正序；校验归属）
async fn load_conversation_history(
    pool: &SqlitePool,
    username: &str,
    conversation_id: i64,
    limit: i64,
) -> Vec<DeepSeekMessage> {
    let owns: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM conversations WHERE id = ? AND username = ?")
            .bind(conversation_id)
            .bind(username)
            .fetch_one(pool)
            .await
            .unwrap_or(0);
    if owns == 0 {
        return Vec::new();
    }
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT role, content FROM (
            SELECT id, role, content FROM conversation_messages
            WHERE conversation_id = ? AND role IN ('user', 'assistant')
            ORDER BY id DESC LIMIT ?
        ) ORDER BY id ASC",
    )
    .bind(conversation_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows.into_iter()
        .map(|(role, content)| DeepSeekMessage { role, content })
        .collect()
}

/// GET /api/conversations/{id}/messages — 加载对话消息历史（仅归属用户）
pub async fn get_conversation_messages(
    Extension(pool): Extension<SqlitePool>,
    Extension(session): Extension<UserSession>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> impl IntoResponse {
    let owns: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM conversations WHERE id = ? AND username = ?")
            .bind(id)
            .bind(&session.username)
            .fetch_one(&pool)
            .await
            .unwrap_or(0);
    if owns == 0 {
        return (StatusCode::NOT_FOUND, "对话不存在").into_response();
    }
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT role, content FROM conversation_messages WHERE conversation_id = ? ORDER BY id ASC",
    )
    .bind(id)
    .fetch_all(&pool)
    .await
    .unwrap_or_default();
    let messages: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(role, content)| serde_json::json!({ "role": role, "content": content }))
        .collect();
    Json(messages).into_response()
}

// ====== DeepSeek API 调用辅助函数 ======

async fn call_deepseek(
    api_base: &str,
    api_key: &str,
    model: &str,
    messages: Vec<DeepSeekMessage>,
    temperature: f32,
    max_tokens: u32,
) -> Result<DeepSeekResponse, (StatusCode, String)> {
    let ds_request = DeepSeekRequest {
        model: model.to_string(),
        messages,
        temperature,
        max_tokens,
        stream: false,
    };

    let url = format!("{}/v1/chat/completions", api_base.trim_end_matches('/'));

    match HTTP_CLIENT
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&ds_request)
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                response.json::<DeepSeekResponse>().await.map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("AI 响应解析失败: {}", e),
                    )
                })
            } else {
                let err_msg = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "未知错误".to_string());
                let user_msg = match status.as_u16() {
                    401 => "AI API 密钥无效".to_string(),
                    429 => "AI 请求过于频繁，请稍后重试".to_string(),
                    500..=599 => "AI 服务暂时不可用".to_string(),
                    _ => {
                        // 上游响应体只进日志，不回显客户端（防错误细节泄露）
                        tracing::error!("[Agent] AI API 错误 status={}: {}", status, err_msg);
                        "AI 服务返回错误，请稍后重试".to_string()
                    }
                };
                Err((StatusCode::BAD_GATEWAY, user_msg))
            }
        }
        Err(e) => {
            let msg = if e.is_timeout() {
                "AI 响应超时".to_string()
            } else {
                tracing::error!("[Agent] API 连接失败: {}", e);
                "AI 服务暂时不可用，请稍后重试".to_string()
            };
            Err((StatusCode::BAD_GATEWAY, msg))
        }
    }
}

/// POST /api/agent/chat — AI 对话（代理到 DeepSeek API）
pub async fn agent_chat(
    Extension(pool): Extension<SqlitePool>,
    Extension(config): Extension<Config>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<ChatRequest>,
) -> impl IntoResponse {
    let resolved = match resolve_agent_config(&pool, &config, &session, payload.model.clone()).await
    {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let model = resolved.model.clone();

    // 构建动态系统提示词（包含用户待办/日程数据）
    let system_prompt = get_cached_prompt(&pool, &session.username).await;

    // 构建消息列表：系统提示 + 上下文
    let mut deepseek_messages = Vec::with_capacity(64);
    deepseek_messages.push(DeepSeekMessage {
        role: "system".to_string(),
        content: system_prompt,
    });

    // 本轮用户消息：带 conversation_id 时取 payload 最后一条 user 消息（历史从库中加载，防重复）；
    // 无 conversation_id 时兼容旧调用（前端一次性发完整历史）
    let last_user_msg = payload
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.clone());

    match payload.conversation_id {
        Some(conv_id) => {
            // 校验归属并把库中历史作为上下文
            let history = load_conversation_history(&pool, &session.username, conv_id, 20).await;
            deepseek_messages.extend(history);
            if let Some(um) = &last_user_msg {
                deepseek_messages.push(DeepSeekMessage {
                    role: "user".to_string(),
                    content: um.clone(),
                });
            }
        }
        None => {
            for msg in &payload.messages {
                let role = match msg.role.as_str() {
                    "user" | "assistant" | "system" => msg.role.clone(),
                    _ => continue,
                };
                deepseek_messages.push(DeepSeekMessage {
                    role,
                    content: msg.content.clone(),
                });
            }
        }
    }

    match call_deepseek(
        &resolved.api_base,
        &resolved.api_key,
        &model,
        deepseek_messages,
        payload.temperature.clamp(0.0, 2.0),
        payload.max_tokens.clamp(1, 8192),
    )
    .await
    {
        Ok(ds_resp) => {
            let reply = ds_resp
                .choices
                .first()
                .map(|c| c.message.content.clone())
                .unwrap_or_else(|| "（AI 未返回内容）".to_string());
            let usage = ds_resp.usage.map(|u| UsageInfo {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            });
            tracing::info!(
                "[AI Agent] 用户={} 模型={} tokens_in={} tokens_out={}",
                session.username,
                model,
                usage.as_ref().map_or(0, |u| u.prompt_tokens),
                usage.as_ref().map_or(0, |u| u.completion_tokens)
            );
            // 持久化本轮对话（无对话时自动创建，返回新对话 ID）
            let conv_id = match last_user_msg {
                Some(um) => save_chat_round(
                    &pool,
                    &session.username,
                    payload.conversation_id,
                    &um,
                    &reply,
                )
                .await
                .ok(),
                None => payload.conversation_id,
            };
            Json(ChatResponse {
                reply,
                model,
                usage,
                conversation_id: conv_id,
            })
            .into_response()
        }
        Err(e) => e.into_response(),
    }
}

// ====== 每日简报生成 ======

#[derive(Debug, Deserialize)]
pub struct BriefingRequest {
    pub todos: Vec<BriefingTodoItem>,
    pub date: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct BriefingTodoItem {
    pub title: String,
    pub description: Option<String>,
    pub due_date: Option<String>,
    pub priority: String,
    pub status: String,
    pub category: String,
}

/// POST /api/agent/briefing — 根据待办/日程生成每日简报
pub async fn generate_briefing(
    Extension(pool): Extension<SqlitePool>,
    Extension(config): Extension<Config>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<BriefingRequest>,
) -> impl IntoResponse {
    let resolved = match resolve_agent_config(&pool, &config, &session, payload.model.clone()).await
    {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };

    let model = resolved.model.clone();

    // 构建提示
    let date_str = payload.date.as_deref().unwrap_or("今天");
    let mut prompt = format!(
        "## 任务：生成 {} 每日简报\n\n以下是用户当天的待办事项和日程安排：\n\n",
        date_str
    );

    // 分类整理
    let todos: Vec<_> = payload
        .todos
        .iter()
        .filter(|t| t.category == "todo")
        .collect();
    let schedules: Vec<_> = payload
        .todos
        .iter()
        .filter(|t| t.category == "schedule")
        .collect();

    if !todos.is_empty() {
        prompt.push_str("### 待办事项\n\n");
        for (i, t) in todos.iter().enumerate() {
            let status_icon = match t.status.as_str() {
                "completed" => "[完成]",
                "in_progress" => "[进行中]",
                _ => "[待办]",
            };
            let priority_icon = match t.priority.as_str() {
                "high" => "[高]",
                "medium" => "[中]",
                _ => "[低]",
            };
            prompt.push_str(&format!(
                "{}. {} {} **{}**",
                i + 1,
                status_icon,
                priority_icon,
                t.title
            ));
            if let Some(ref desc) = t.description
                && !desc.trim().is_empty()
            {
                prompt.push_str(&format!(" — {}", desc));
            }
            if let Some(ref due) = t.due_date {
                prompt.push_str(&format!(" (截止: {})", due));
            }
            prompt.push('\n');
        }
        prompt.push('\n');
    }

    if !schedules.is_empty() {
        prompt.push_str("### 日程安排\n\n");
        // 按日期排序
        let mut sorted_schedules = schedules.clone();
        sorted_schedules.sort_by(|a, b| a.due_date.cmp(&b.due_date));
        for (i, s) in sorted_schedules.iter().enumerate() {
            prompt.push_str(&format!("{}. **{}**", i + 1, s.title));
            if let Some(ref desc) = s.description
                && !desc.trim().is_empty()
            {
                prompt.push_str(&format!(" — {}", desc));
            }
            if let Some(ref due) = s.due_date {
                prompt.push_str(&format!(" [日期: {}]", due));
            }
            prompt.push('\n');
        }
        prompt.push('\n');
    }

    if todos.is_empty() && schedules.is_empty() {
        prompt.push_str("（暂无待办事项和日程安排）\n\n请生成一条鼓励性的消息，建议用户添加一些待办事项或日程。\n");
    }

    prompt.push_str(
        "\
---
请生成一份专业的每日简报，包含：
1. **今日概览** — 简要总结今日安排
2. **优先事项** — 列出最紧急重要的 3 件事
3. **时间建议** — 给出合理的时间安排建议
4. **小贴士** — 一条关于提高效率的建议
5. **总结** — 一句鼓励的话

使用 Markdown 格式，风格专业且温暖。",
    );

    let messages = vec![DeepSeekMessage {
        role: "user".to_string(),
        content: prompt,
    }];

    match call_deepseek(
        &resolved.api_base,
        &resolved.api_key,
        &model,
        messages,
        resolved.temperature,
        resolved.max_tokens,
    )
    .await
    {
        Ok(ds_resp) => {
            let briefing = ds_resp
                .choices
                .first()
                .map(|c| c.message.content.clone())
                .unwrap_or_else(|| "（AI 未返回内容）".to_string());
            tracing::info!("[AI Agent] 简报生成成功，用户={}", session.username);
            Json(serde_json::json!({ "briefing": briefing, "model": model })).into_response()
        }
        Err(e) => e.into_response(),
    }
}

// ====== HTMX Fragment 处理器 ======
use crate::templates::AppTemplate;
use askama::Template;

/// 聊天消息 HTML 片段（用户消息 + AI 回复）
#[derive(Template)]
#[template(path = "components/chat_message.html")]
struct ChatMessageFragment {
    user_message: String,
    assistant_reply: String,
    model: String,
    usage_info: String,
}

/// 简报结果 HTML 片段
#[derive(Template)]
#[template(path = "components/briefing_result.html")]
struct BriefingFragment {
    content: String,
    model: String,
}

/// POST /agent/chat — HTMX 聊天（Form 数据）
#[tracing::instrument(skip_all)]
pub async fn agent_chat_fragment(
    Extension(pool): Extension<SqlitePool>,
    Extension(config): Extension<Config>,
    Extension(session): Extension<UserSession>,
    axum::extract::Form(form): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let message = form.get("message").cloned().unwrap_or_default();
    if message.trim().is_empty() {
        return AppTemplate(ChatMessageFragment {
            user_message: String::new(),
            assistant_reply: "请输入消息".to_string(),
            model: String::new(),
            usage_info: String::new(),
        })
        .into_response();
    }

    let user_msg = message.trim().to_string();
    let model_override = form.get("model").cloned().filter(|m| !m.is_empty());

    let resolved = match resolve_agent_config(&pool, &config, &session, model_override).await {
        Ok(r) => r,
        Err((_, msg)) => {
            return AppTemplate(ChatMessageFragment {
                user_message: user_msg,
                assistant_reply: format!("配置错误: {}", msg),
                model: String::new(),
                usage_info: String::new(),
            })
            .into_response();
        }
    };

    let model = resolved.model.clone();
    let system_prompt = get_cached_prompt(&pool, &session.username).await;

    // 会话 ID（表单 hidden 字段）：有值则加载库历史作为上下文
    let conversation_id: Option<i64> = form.get("conversation_id").and_then(|s| s.parse().ok());

    let mut messages = Vec::with_capacity(32);
    messages.push(DeepSeekMessage {
        role: "system".to_string(),
        content: system_prompt,
    });
    if let Some(cid) = conversation_id {
        let history = load_conversation_history(&pool, &session.username, cid, 20).await;
        messages.extend(history);
    }
    messages.push(DeepSeekMessage {
        role: "user".to_string(),
        content: user_msg.clone(),
    });

    match call_deepseek(
        &resolved.api_base,
        &resolved.api_key,
        &model,
        messages,
        resolved.temperature,
        resolved.max_tokens,
    )
    .await
    {
        Ok(ds_resp) => {
            let reply = ds_resp
                .choices
                .first()
                .map(|c| c.message.content.clone())
                .unwrap_or_else(|| "（AI 未返回内容）".to_string());
            let usage = ds_resp
                .usage
                .map(|u| {
                    format!(
                        "tokens: {} in / {} out",
                        u.prompt_tokens, u.completion_tokens
                    )
                })
                .unwrap_or_default();
            // 持久化本轮消息（无对话时自动创建，标题取首条消息）
            let _ =
                save_chat_round(&pool, &session.username, conversation_id, &user_msg, &reply).await;
            AppTemplate(ChatMessageFragment {
                user_message: user_msg,
                assistant_reply: reply,
                model,
                usage_info: usage,
            })
            .into_response()
        }
        Err((_, msg)) => AppTemplate(ChatMessageFragment {
            user_message: user_msg,
            assistant_reply: format!("请求失败: {}", msg),
            model,
            usage_info: String::new(),
        })
        .into_response(),
    }
}

/// POST /agent/briefing — HTMX 简报生成（Form 数据）
#[tracing::instrument(skip_all)]
pub async fn agent_briefing_fragment(
    Extension(pool): Extension<SqlitePool>,
    Extension(config): Extension<Config>,
    Extension(session): Extension<UserSession>,
    axum::extract::Form(form): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let model_override = form.get("model").cloned().filter(|m| !m.is_empty());

    let resolved = match resolve_agent_config(&pool, &config, &session, model_override).await {
        Ok(r) => r,
        Err((_, msg)) => {
            return AppTemplate(BriefingFragment {
                content: format!("配置错误: {}", msg),
                model: String::new(),
            })
            .into_response();
        }
    };
    let model = resolved.model.clone();

    let todos: Vec<crate::handlers::TodoItem> = sqlx::query_as(
        "SELECT id, username, title, description, due_date, is_all_day, start_time, end_time, priority, status, category, created_at, updated_at FROM todos WHERE username = ? AND status != 'completed' ORDER BY due_date ASC NULLS LAST"
    )
    .bind(&session.username)
    .fetch_all(&pool)
    .await
    .unwrap_or_default();

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut prompt = format!("## 任务：生成 {} 每日简报\n\n", today);

    let todo_items: Vec<_> = todos.iter().filter(|t| t.category == "todo").collect();
    let schedule_items: Vec<_> = todos.iter().filter(|t| t.category == "schedule").collect();

    if !todo_items.is_empty() {
        prompt.push_str("### 待办事项\n\n");
        for (i, t) in todo_items.iter().enumerate() {
            prompt.push_str(&format!("{}. **{}**", i + 1, t.title));
            if let Some(ref desc) = t.description
                && !desc.trim().is_empty()
            {
                prompt.push_str(&format!(" — {}", desc));
            }
            if let Some(ref due) = t.due_date {
                prompt.push_str(&format!(" (截止: {})", due));
            }
            prompt.push('\n');
        }
        prompt.push('\n');
    }

    if !schedule_items.is_empty() {
        prompt.push_str("### 日程安排\n\n");
        for (i, s) in schedule_items.iter().enumerate() {
            prompt.push_str(&format!(
                "{}. **{}** (日期: {})",
                i + 1,
                s.title,
                s.due_date.as_deref().unwrap_or("未知")
            ));
            prompt.push('\n');
        }
        prompt.push('\n');
    }

    if todo_items.is_empty() && schedule_items.is_empty() {
        prompt.push_str("（暂无待办事项和日程安排）\n\n请生成一条鼓励性的消息。\n");
    }

    prompt.push_str("\n---\n请生成一份专业的每日简报，包含今日概览、优先事项、时间建议和一句鼓励的话。使用 Markdown 格式。");

    let messages = vec![DeepSeekMessage {
        role: "user".to_string(),
        content: prompt,
    }];

    match call_deepseek(
        &resolved.api_base,
        &resolved.api_key,
        &model,
        messages,
        resolved.temperature,
        resolved.max_tokens,
    )
    .await
    {
        Ok(ds_resp) => {
            let briefing = ds_resp
                .choices
                .first()
                .map(|c| c.message.content.clone())
                .unwrap_or_else(|| "（AI 未返回内容）".to_string());
            AppTemplate(BriefingFragment {
                content: briefing,
                model,
            })
            .into_response()
        }
        Err((_, msg)) => AppTemplate(BriefingFragment {
            content: format!("简报生成失败: {}", msg),
            model,
        })
        .into_response(),
    }
}

/// GET /agent/settings-form — Agent 设置模态框
#[derive(Template)]
#[template(path = "components/settings_form.html")]
struct SettingsFormFragment {
    api_key_placeholder: String,
    api_key_hint: String,
    api_base: String,
    model: String,
    temperature: f32,
    max_tokens: u32,
}

pub async fn agent_settings_form(
    Extension(pool): Extension<SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> impl IntoResponse {
    let row: UserSettingsRow = sqlx::query_as(
        "SELECT deepseek_api_key, deepseek_api_base, deepseek_model, temperature, max_tokens FROM user_settings WHERE username = ?"
    )
    .bind(&session.username)
    .fetch_optional(&pool)
    .await
    .unwrap_or(None);

    let (has_key, api_base, model, temperature, max_tokens) = match row {
        Some((key, base, m, t, mt)) => (
            key.is_some(),
            base.unwrap_or_default(),
            m.unwrap_or_default(),
            t.unwrap_or(0.7),
            mt.unwrap_or(4096),
        ),
        None => (false, String::new(), String::new(), 0.7f32, 4096u32),
    };

    AppTemplate(SettingsFormFragment {
        api_key_placeholder: if has_key {
            "已配置（输入新值覆盖）".into()
        } else {
            "sk-...".into()
        },
        api_key_hint: if has_key {
            "已配置 API Key（已脱敏保存）".into()
        } else {
            "留空则使用服务器全局设置".into()
        },
        api_base,
        model,
        temperature,
        max_tokens,
    })
}
