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
        Some((api_key, api_base, model, temp, tokens)) => {
            // 落库密文 → 解密后掩码回显（解密失败按未配置处理并告警，绝不明文外泄损坏数据）
            let decrypted = api_key.as_deref().map(|k| {
                crate::core::secrets::decrypt_secret(k).unwrap_or_else(|e| {
                    tracing::error!("[Settings] API Key 解密失败: {}", e);
                    String::new()
                })
            });
            Ok(Json(serde_json::json!({
                "deepseek_api_key": decrypted.as_ref().map(|k| mask_api_key(k)),
                "deepseek_api_key_configured": decrypted.as_ref().is_some_and(|k| !k.is_empty()),
                "deepseek_api_base": api_base,
                "deepseek_model": model,
                "temperature": temp.unwrap_or(0.7),
                "max_tokens": tokens.unwrap_or(4096),
            })))
        }
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

/// 深度校验自定义 api_base（M1/SSRF）：写入与读取（resolve）共用，防历史/异常数据绕过。
/// - 仅 https
/// - 拒绝 IP 直连（含 IPv6 字面量）
/// - 拒绝 localhost/.local/.internal 与私网/链路本地前缀
/// - 拒绝常见 DNS 重绑定域名后缀（nip.io/sslip.io/xip.io/localtest.me）
///
/// 残余风险：任意域名仍可在请求时重绑定到私网（需 DNS 钉扎才能根除），已在 CLAUDE.md 记录
pub fn validate_api_base(base: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(base).map_err(|_| "API 基础地址格式非法".to_string())?;
    if url.scheme() != "https" {
        return Err("API 基础地址必须使用 https://（拒绝明文与本地传输）".to_string());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "API 基础地址缺少主机名".to_string())?
        .to_ascii_lowercase();
    let is_literal_ip = host.parse::<std::net::IpAddr>().is_ok();
    let is_local = host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.starts_with("127.")
        || host.starts_with("10.")
        || host.starts_with("192.168.")
        || host.starts_with("169.254.")
        || host.starts_with("172.16.")
        || host.starts_with("172.17.")
        || host.starts_with("172.18.")
        || host.starts_with("172.19.")
        || host.starts_with("172.2")
        || host.starts_with("172.30.")
        || host.starts_with("172.31.")
        || host.starts_with("100.")
        || host.starts_with("0.");
    let is_rebinding = host.ends_with(".nip.io")
        || host.ends_with(".sslip.io")
        || host.ends_with(".xip.io")
        || host.ends_with(".localtest.me");
    if is_literal_ip || is_local || is_rebinding {
        return Err("API 基础地址不允许使用 IP 地址、本地主机或重绑定域名".to_string());
    }
    Ok(())
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
        && let Err(msg) = validate_api_base(base)
    {
        return Err(AppError::bad_request(msg));
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

    // 首次保存（无现有行）时 temperature/max_tokens 为 NULL 会撞 NOT NULL 约束 → 500：
    // INSERT 侧同样 COALESCE 兜底（与 ON CONFLICT 侧口径一致，部分字段保存可用）
    // P0-3：API Key 落库前 ChaCha20-Poly1305 加密；空串 = 清除密钥（明文空串不受影响）
    let api_key_encrypted = payload.deepseek_api_key.as_deref().map(|k| {
        if k.is_empty() {
            String::new()
        } else {
            crate::core::secrets::encrypt_secret(k)
        }
    });
    sqlx::query(
        "INSERT INTO user_settings (username, deepseek_api_key, deepseek_api_base, deepseek_model, temperature, max_tokens)
         VALUES (?, ?, ?, ?, COALESCE(?, 0.7), COALESCE(?, 4096))
         ON CONFLICT(username) DO UPDATE SET
             deepseek_api_key = CASE WHEN excluded.deepseek_api_key IS NULL THEN user_settings.deepseek_api_key ELSE NULLIF(excluded.deepseek_api_key, '') END,
             deepseek_api_base = CASE WHEN excluded.deepseek_api_base IS NULL THEN user_settings.deepseek_api_base ELSE NULLIF(excluded.deepseek_api_base, '') END,
             deepseek_model   = CASE WHEN excluded.deepseek_model   IS NULL THEN user_settings.deepseek_model   ELSE NULLIF(excluded.deepseek_model, '') END,
             temperature      = COALESCE(excluded.temperature, user_settings.temperature, 0.7),
             max_tokens       = COALESCE(excluded.max_tokens, user_settings.max_tokens, 4096)"
    )
    .bind(&session.username)
    .bind(&api_key_encrypted)
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
