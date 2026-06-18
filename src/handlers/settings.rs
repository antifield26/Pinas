// ====== AI Agent 用户设置 ======
use axum::{
    extract::Extension,
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::Deserialize;
use sqlx::SqlitePool;

use pinas_core::UserSession;

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
) -> impl IntoResponse {
    let row = sqlx::query_as::<_, (Option<String>, Option<String>, Option<String>)>(
        "SELECT deepseek_api_key, deepseek_api_base, deepseek_model
         FROM user_settings WHERE username = ?"
    )
    .bind(&session.username)
    .fetch_optional(&pool)
    .await;

    match row {
        Ok(Some((api_key, api_base, model))) => {
            // 返回 API key 时做脱敏处理
            let key_configured = api_key.as_ref().map_or(false, |k| !k.is_empty());
            let masked_key = api_key.map(|k| mask_api_key(&k));
            Json(serde_json::json!({
                "deepseek_api_key": masked_key,
                "deepseek_api_key_configured": key_configured,
                "deepseek_api_base": api_base,
                "deepseek_model": model,
            })).into_response()
        }
        Ok(None) => {
            Json(serde_json::json!({
                "deepseek_api_key": null,
                "deepseek_api_key_configured": false,
                "deepseek_api_base": null,
                "deepseek_model": null,
            })).into_response()
        }
        Err(e) => {
            tracing::error!("[Settings] 查询用户设置失败: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "查询设置失败").into_response()
        }
    }
}

/// PUT /api/agent/settings — 保存当前用户的 AI Agent 设置
pub async fn save_agent_settings(
    Extension(pool): Extension<SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<SaveAgentSettingsRequest>,
) -> impl IntoResponse {
    // 基本校验
    if let Some(ref base) = payload.deepseek_api_base {
        if !base.is_empty() && !base.starts_with("http") {
            return (StatusCode::BAD_REQUEST, "API 基础地址必须以 http:// 或 https:// 开头").into_response();
        }
    }

    let result = sqlx::query(
        "INSERT INTO user_settings (username, deepseek_api_key, deepseek_api_base, deepseek_model)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(username) DO UPDATE SET
             deepseek_api_key = COALESCE(excluded.deepseek_api_key, user_settings.deepseek_api_key),
             deepseek_api_base = COALESCE(excluded.deepseek_api_base, user_settings.deepseek_api_base),
             deepseek_model = COALESCE(excluded.deepseek_model, user_settings.deepseek_model)"
    )
    .bind(&session.username)
    .bind(&payload.deepseek_api_key)
    .bind(&payload.deepseek_api_base)
    .bind(&payload.deepseek_model)
    .execute(&pool)
    .await;

    match result {
        Ok(_) => {
            tracing::info!("[Settings] 用户 {} 已更新 AI Agent 设置", session.username);
            Json(serde_json::json!({ "ok": true })).into_response()
        }
        Err(e) => {
            tracing::error!("[Settings] 保存用户设置失败: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "保存设置失败").into_response()
        }
    }
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
