// ====== AI Agent 用户设置 ======
use axum::{
    extract::Extension,
    response::Json,
};
use serde::Deserialize;
use sqlx::SqlitePool;

use pinas_core::UserSession;
use crate::error::{AppError, AppResult};

// ====== DTOs ======

#[derive(Debug, Deserialize)]
pub struct SaveAgentSettingsRequest {
    pub deepseek_api_key: Option<String>,
    pub deepseek_api_base: Option<String>,
    pub deepseek_model: Option<String>,
}

// ====== 处理器 ======

/// GET /api/agent/settings — 获取当前用户的 AI Agent 设置
pub async fn get_agent_settings(
    Extension(pool): Extension<SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> AppResult<Json<serde_json::Value>> {
    let row = sqlx::query_as::<_, (Option<String>, Option<String>, Option<String>)>(
        "SELECT deepseek_api_key, deepseek_api_base, deepseek_model
         FROM user_settings WHERE username = ?"
    )
    .bind(&session.username)
    .fetch_optional(&pool)
    .await?;

    match row {
        Some((api_key, api_base, model)) => {
            let key_configured = api_key.as_ref().is_some_and(|k| !k.is_empty());
            let masked_key = api_key.map(|k| mask_api_key(&k));
            Ok(Json(serde_json::json!({
                "deepseek_api_key": masked_key,
                "deepseek_api_key_configured": key_configured,
                "deepseek_api_base": api_base,
                "deepseek_model": model,
            })))
        }
        None => {
            Ok(Json(serde_json::json!({
                "deepseek_api_key": null,
                "deepseek_api_key_configured": false,
                "deepseek_api_base": null,
                "deepseek_model": null,
            })))
        }
    }
}

/// PUT /api/agent/settings — 保存当前用户的 AI Agent 设置
pub async fn save_agent_settings(
    Extension(pool): Extension<SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<SaveAgentSettingsRequest>,
) -> AppResult<Json<serde_json::Value>> {
    // 基本校验
    if let Some(ref base) = payload.deepseek_api_base {
        if !base.is_empty() && !base.starts_with("http") {
            return Err(AppError::bad_request("API 基础地址必须以 http:// 或 https:// 开头"));
        }
    }

    sqlx::query(
        "INSERT INTO user_settings (username, deepseek_api_key, deepseek_api_base, deepseek_model)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(username) DO UPDATE SET
             deepseek_api_key = CASE
                 WHEN excluded.deepseek_api_key IS NULL THEN user_settings.deepseek_api_key
                 ELSE NULLIF(excluded.deepseek_api_key, '')
             END,
             deepseek_api_base = CASE
                 WHEN excluded.deepseek_api_base IS NULL THEN user_settings.deepseek_api_base
                 ELSE NULLIF(excluded.deepseek_api_base, '')
             END,
             deepseek_model = CASE
                 WHEN excluded.deepseek_model IS NULL THEN user_settings.deepseek_model
                 ELSE NULLIF(excluded.deepseek_model, '')
             END"
    )
    .bind(&session.username)
    .bind(&payload.deepseek_api_key)
    .bind(&payload.deepseek_api_base)
    .bind(&payload.deepseek_model)
    .execute(&pool)
    .await?;

    tracing::info!("[Settings] 用户 {} 已更新 AI Agent 设置", session.username);
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// 对 API key 进行脱敏：保留前6位和后4位
fn mask_api_key(key: &str) -> String {
    if key.len() <= 8 {
        return "****".to_string();
    }
    let prefix: String = key.chars().take(6).collect();
    let suffix: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{}****{}", prefix, suffix)
}
