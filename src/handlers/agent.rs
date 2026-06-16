use axum::{
    extract::Extension,
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::sync::LazyLock;

use pinas_core::UserSession;
use crate::config::Config;

// ====== 共享 HTTP 客户端 ======
static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("Failed to create HTTP client")
});

// ====== 模型列表 ======
const AVAILABLE_MODELS: &[(&str, &str)] = &[
    ("deepseek-v4-pro[1m]", "DeepSeek V4 Pro (推荐)[1M]"),
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
}

fn default_temperature() -> f32 { 0.7 }
fn default_max_tokens() -> u32 { 4096 }

#[derive(Debug, Serialize)]
pub struct ChatResponse {
    pub reply: String,
    pub model: String,
    pub usage: Option<UsageInfo>,
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
const SYSTEM_PROMPT: &str = r#"你是 Antifield Cloud 的 AI 智能助手。你可以帮助用户：

1. **任务管理** — 帮助整理待办事项，建议优先级排序
2. **日程规划** — 根据日程生成合理的每日计划
3. **每日简报** — 汇总用户的待办和日程，生成清晰的每日简报
4. **文件建议** — 提供文件组织和管理建议
5. **通用问答** — 回答各类问题

请用中文回复，风格简洁专业、友好亲切。回复使用 Markdown 格式。"#;

// ====== 处理器 ======

/// 解析用户 Agent 配置：用户设置 > 全局配置
struct ResolvedAgentConfig {
    api_key: String,
    api_base: String,
    model: String,
}

async fn resolve_agent_config(
    pool: &SqlitePool,
    config: &Config,
    session: &UserSession,
    requested_model: Option<String>,
) -> Result<ResolvedAgentConfig, (StatusCode, String)> {
    // 尝试读取用户设置
    let user_settings: Option<(Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT deepseek_api_key, deepseek_api_base, deepseek_model FROM user_settings WHERE username = ?"
    )
    .bind(&session.username)
    .fetch_optional(pool)
    .await
    .unwrap_or(None);

    let user_api_key = user_settings.as_ref().and_then(|u| u.0.clone());
    let user_api_base = user_settings.as_ref().and_then(|u| u.1.clone());
    let user_model = user_settings.as_ref().and_then(|u| u.2.clone());

    // API Key: 用户设置 > 全局配置
    let api_key = match user_api_key.or_else(|| config.deepseek_api_key.clone()) {
        Some(k) if !k.is_empty() => k,
        _ => return Err((StatusCode::SERVICE_UNAVAILABLE,
            "AI 服务未配置。请在 Agent 设置中配置 API Key 或设置环境变量 PINAS_DEEPSEEK_API_KEY".to_string())),
    };

    // API Base: 用户设置 > 全局配置 > 默认值
    let api_base = user_api_base
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| config.deepseek_api_base.clone());

    // Model: 请求参数 > 用户设置 > 全局配置
    let model = requested_model
        .filter(|m| !m.is_empty())
        .or_else(|| user_model.filter(|m| !m.is_empty()))
        .unwrap_or_else(|| config.deepseek_model.clone());

    Ok(ResolvedAgentConfig { api_key, api_base, model })
}

/// GET /api/agent/models — 返回可用模型列表
pub async fn get_models() -> impl IntoResponse {
    let models: Vec<serde_json::Value> = AVAILABLE_MODELS.iter().map(|(id, name)| {
        serde_json::json!({ "id": id, "name": name })
    }).collect();
    Json(models)
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
                    (StatusCode::INTERNAL_SERVER_ERROR, format!("AI 响应解析失败: {}", e))
                })
            } else {
                let err_msg = response.text().await.unwrap_or_else(|_| "未知错误".to_string());
                let user_msg = match status.as_u16() {
                    401 => "AI API 密钥无效".to_string(),
                    429 => "AI 请求过于频繁，请稍后重试".to_string(),
                    500..=599 => "AI 服务暂时不可用".to_string(),
                    _ => format!("AI 服务错误: {}", err_msg),
                };
                Err((StatusCode::BAD_GATEWAY, user_msg))
            }
        }
        Err(e) => {
            let msg = if e.is_timeout() {
                "AI 响应超时".to_string()
            } else {
                format!("连接 AI 服务失败: {}", e)
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
    let resolved = match resolve_agent_config(&pool, &config, &session, payload.model.clone()).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    let model = resolved.model.clone();

    // 构建消息列表：系统提示 + 用户消息历史
    let mut deepseek_messages = Vec::with_capacity(payload.messages.len() + 1);
    deepseek_messages.push(DeepSeekMessage {
        role: "system".to_string(),
        content: SYSTEM_PROMPT.to_string(),
    });

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

    match call_deepseek(
        &resolved.api_base,
        &resolved.api_key,
        &model,
        deepseek_messages,
        payload.temperature.clamp(0.0, 2.0),
        payload.max_tokens.clamp(1, 8192),
    ).await {
        Ok(ds_resp) => {
            let reply = ds_resp.choices
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
            Json(ChatResponse { reply, model, usage }).into_response()
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
    let resolved = match resolve_agent_config(&pool, &config, &session, payload.model.clone()).await {
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
    let todos: Vec<_> = payload.todos.iter().filter(|t| t.category == "todo").collect();
    let schedules: Vec<_> = payload.todos.iter().filter(|t| t.category == "schedule").collect();

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
                i + 1, status_icon, priority_icon, t.title
            ));
            if let Some(ref desc) = t.description {
                if !desc.trim().is_empty() {
                    prompt.push_str(&format!(" — {}", desc));
                }
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
            prompt.push_str(&format!(
                "{}. **{}**",
                i + 1, s.title
            ));
            if let Some(ref desc) = s.description {
                if !desc.trim().is_empty() {
                    prompt.push_str(&format!(" — {}", desc));
                }
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

    prompt.push_str("\
---
请生成一份专业的每日简报，包含：
1. **今日概览** — 简要总结今日安排
2. **优先事项** — 列出最紧急重要的 3 件事
3. **时间建议** — 给出合理的时间安排建议
4. **小贴士** — 一条关于提高效率的建议
5. **总结** — 一句鼓励的话

使用 Markdown 格式，风格专业且温暖。");

    let messages = vec![DeepSeekMessage {
        role: "user".to_string(),
        content: prompt,
    }];

    match call_deepseek(&resolved.api_base, &resolved.api_key, &model, messages, 0.7, 2048).await {
        Ok(ds_resp) => {
            let briefing = ds_resp.choices
                .first()
                .map(|c| c.message.content.clone())
                .unwrap_or_else(|| "（AI 未返回内容）".to_string());
            tracing::info!("[AI Agent] 简报生成成功，用户={}", session.username);
            Json(serde_json::json!({ "briefing": briefing, "model": model })).into_response()
        }
        Err(e) => e.into_response(),
    }
}
