// ====== AI Agent 用户设置 ======
use axum::{extract::Extension, response::Json};
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::core::UserSession;
use crate::error::{AppError, AppResult};

// ====== DTOs ======

#[derive(Debug, Deserialize)]
pub struct SaveAgentSettingsRequest {
    pub deepseek_api_key: Option<String>,
    pub deepseek_api_base: Option<String>,
    pub deepseek_model: Option<String>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

// ====== 处理器 ======

/// GET /api/agent/settings
#[tracing::instrument(skip_all)]
pub async fn get_agent_settings(
    Extension(pool): Extension<SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> AppResult<Json<serde_json::Value>> {
    let row = sqlx::query_as::<
        _,
        (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<f32>,
            Option<u32>,
        ),
    >(
        "SELECT deepseek_api_key, deepseek_api_base, deepseek_model, temperature, max_tokens
         FROM user_settings WHERE username = ?",
    )
    .bind(&session.username)
    .fetch_optional(&pool)
    .await?;

    match row {
        Some((api_key, api_base, model, temp, tokens)) => Ok(Json(serde_json::json!({
            "deepseek_api_key": api_key.as_ref().map(|k| mask_api_key(k)),
            "deepseek_api_key_configured": api_key.as_ref().is_some_and(|k| !k.is_empty()),
            "deepseek_api_base": api_base,
            "deepseek_model": model,
            "temperature": temp.unwrap_or(0.7),
            "max_tokens": tokens.unwrap_or(4096),
        }))),
        None => Ok(Json(serde_json::json!({
            "deepseek_api_key": null,
            "deepseek_api_key_configured": false,
            "deepseek_api_base": null,
            "deepseek_model": null,
            "temperature": 0.7,
            "max_tokens": 4096,
        }))),
    }
}

/// PUT /api/agent/settings
#[tracing::instrument(skip_all)]
pub async fn save_agent_settings(
    Extension(pool): Extension<SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<SaveAgentSettingsRequest>,
) -> AppResult<Json<serde_json::Value>> {
    if let Some(ref base) = payload.deepseek_api_base
        && !base.is_empty()
    {
        // 仅允许 https，且拒绝 IP 直连与本地地址（防自定义 base 被用于窃取全局 key / 探内网）
        if !base.starts_with("https://") {
            return Err(AppError::bad_request(
                "API 基础地址必须使用 https://（拒绝明文与本地传输）",
            ));
        }
        let host_part = base
            .trim_start_matches("https://")
            .split(['/', '?', ':'])
            .next()
            .unwrap_or("");
        let is_literal_ip = host_part.parse::<std::net::IpAddr>().is_ok();
        let is_local_host = host_part.eq_ignore_ascii_case("localhost")
            || host_part.ends_with(".localhost")
            || host_part.starts_with("127.")
            || host_part.starts_with("10.")
            || host_part.starts_with("192.168.")
            || host_part.ends_with(".local")
            || host_part.ends_with(".internal");
        if is_literal_ip || is_local_host {
            return Err(AppError::bad_request(
                "API 基础地址不允许使用 IP 地址或本地主机",
            ));
        }
    }
    if let Some(t) = payload.temperature
        && !(0.0..=2.0).contains(&t)
    {
        return Err(AppError::bad_request("Temperature 必须在 0.0 到 2.0 之间"));
    }
    if let Some(m) = payload.max_tokens
        && (!(1..=8192).contains(&m))
    {
        return Err(AppError::bad_request("Max tokens 必须在 1 到 8192 之间"));
    }

    sqlx::query(
        "INSERT INTO user_settings (username, deepseek_api_key, deepseek_api_base, deepseek_model, temperature, max_tokens)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(username) DO UPDATE SET
             deepseek_api_key = CASE WHEN excluded.deepseek_api_key IS NULL THEN user_settings.deepseek_api_key ELSE NULLIF(excluded.deepseek_api_key, '') END,
             deepseek_api_base = CASE WHEN excluded.deepseek_api_base IS NULL THEN user_settings.deepseek_api_base ELSE NULLIF(excluded.deepseek_api_base, '') END,
             deepseek_model   = CASE WHEN excluded.deepseek_model   IS NULL THEN user_settings.deepseek_model   ELSE NULLIF(excluded.deepseek_model, '') END,
             temperature      = COALESCE(excluded.temperature, user_settings.temperature, 0.7),
             max_tokens       = COALESCE(excluded.max_tokens, user_settings.max_tokens, 4096)"
    )
    .bind(&session.username)
    .bind(&payload.deepseek_api_key)
    .bind(&payload.deepseek_api_base)
    .bind(&payload.deepseek_model)
    .bind(payload.temperature)
    .bind(payload.max_tokens.map(|m| m as i64))
    .execute(&pool)
    .await?;

    tracing::info!("[Settings] 用户 {} 已更新 AI Agent 设置", session.username);
    Ok(Json(serde_json::json!({ "ok": true })))
}

fn mask_api_key(key: &str) -> String {
    if key.len() <= 8 {
        return "****".to_string();
    }
    let prefix: String = key.chars().take(6).collect();
    let suffix: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{}****{}", prefix, suffix)
}
