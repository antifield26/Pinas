use axum::{
    extract::Extension,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
};
use argon2::{Argon2, PasswordHash, PasswordVerifier};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::handlers::utils::{log_audit};
use crate::config::Config;
use crate::handlers::rate_limit;
use crate::constants::*;
use pinas_core::hash_token;

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub username: String,
    pub role: String,
    pub must_change_pwd: bool,
}

/// 从 HTTP 请求头提取客户端 IP（优先 X-Forwarded-For）
fn extract_ip(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|s| s.trim())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
        })
}

/// 从 HTTP 请求头提取 User-Agent
fn extract_ua(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
}

#[tracing::instrument(skip(pool, config, headers, payload))]
pub async fn login(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(config): Extension<Config>,
    headers: HeaderMap,
    Json(payload): Json<LoginRequest>,
) -> impl IntoResponse {
    // 速率限制：每个 IP 每分钟最多 N 次登录尝试
    if let Some(ip) = extract_ip(&headers) {
        if !rate_limit::check_rate_limit(ip, LOGIN_RATE_LIMIT_ATTEMPTS, std::time::Duration::from_secs(LOGIN_RATE_LIMIT_WINDOW_SECS)) {
            return (StatusCode::TOO_MANY_REQUESTS, "登录尝试过于频繁，请稍后再试").into_response();
        }
    }

    let row = sqlx::query("SELECT password, role, must_change_pwd FROM users WHERE username = ?")
        .bind(&payload.username)
        .fetch_optional(&pool)
        .await
        .unwrap_or(None);

    if let Some(r) = row {
        let db_hash: String = r.get("password");
        let role: String = r.get("role");
        let must_change: i64 = r.get("must_change_pwd");
        let password = payload.password.clone();

        // Argon2 验证放入 spawn_blocking 避免阻塞 async 线程
        let is_valid = tokio::task::spawn_blocking(move || {
            match PasswordHash::new(&db_hash) {
                Ok(parsed_hash) => Argon2::default()
                    .verify_password(password.as_bytes(), &parsed_hash)
                    .is_ok(),
                Err(e) => {
                    tracing::error!("[Login] 密码哈希解析失败: {}", e);
                    false
                }
            }
        }).await.unwrap_or(false);

        if is_valid {
            let token = Uuid::new_v4().to_string();
            let token_hash = hash_token(&token);
            let expires = chrono::Utc::now()
                .checked_add_signed(chrono::Duration::days(config.session_days))
                .unwrap_or_else(|| chrono::Utc::now() + chrono::Duration::days(7))
                .format("%Y-%m-%d %H:%M:%S")
                .to_string();

            if let Err(e) = sqlx::query("INSERT INTO sessions (token, username, role, expires_at) VALUES (?, ?, ?, ?)")
                .bind(&token_hash)
                .bind(&payload.username)
                .bind(&role)
                .bind(&expires)
                .execute(&pool)
                .await
            {
                tracing::error!("创建会话失败: {}", e);
                return (StatusCode::INTERNAL_SERVER_ERROR, "创建会话失败，请稍后重试").into_response();
            }

            let _ = log_audit(&pool, &payload.username, "login", None, None, extract_ip(&headers), extract_ua(&headers)).await;

            return (StatusCode::OK, Json(LoginResponse {
                token,
                username: payload.username,
                role,
                must_change_pwd: must_change != 0,
            })).into_response();
        }
        tracing::warn!("[Login] 用户 '{}' 密码验证失败", payload.username);
    } else {
        tracing::warn!("[Login] 用户 '{}' 不存在", payload.username);
    }
    (StatusCode::UNAUTHORIZED, "账号或访问密码校验失败").into_response()
}

#[tracing::instrument(skip(pool, headers, payload))]
pub async fn register(
    Extension(pool): Extension<sqlx::SqlitePool>,
    headers: HeaderMap,
    Json(payload): Json<RegisterRequest>,
) -> impl IntoResponse {
    use crate::handlers::utils::hash_password;

    // 速率限制：每个 IP 每小时最多 N 次注册
    if let Some(ip) = extract_ip(&headers) {
        if !rate_limit::check_rate_limit(ip, REGISTER_RATE_LIMIT_ATTEMPTS, std::time::Duration::from_secs(REGISTER_RATE_LIMIT_WINDOW_SECS)) {
            return (StatusCode::TOO_MANY_REQUESTS, "注册过于频繁，请稍后再试").into_response();
        }
    }

    let user_exists = sqlx::query("SELECT username FROM users WHERE username = ?")
        .bind(&payload.username)
        .fetch_optional(&pool)
        .await
        .unwrap_or(None)
        .is_some();

    if user_exists {
        return (StatusCode::BAD_REQUEST, "用户已被注册").into_response();
    }

    let pwd = payload.password.clone();
    let hashed_password = match tokio::task::spawn_blocking(move || hash_password(&pwd)).await {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "服务内部错误").into_response(),
    };

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap_or(0);
    let role = if count == 0 { ROLE_ADMIN } else { ROLE_USER };

    // 直接 INSERT，依赖 UNIQUE 约束防止 TOCTOU 竞态
    let insert_result = sqlx::query("INSERT INTO users (username, password, role) VALUES (?, ?, ?)")
        .bind(&payload.username)
        .bind(&hashed_password)
        .bind(role)
        .execute(&pool)
        .await;

    if let Err(e) = insert_result {
        if let Some(db_err) = e.as_database_error() {
            if db_err.is_unique_violation() {
                return (StatusCode::CONFLICT, "用户已被注册").into_response();
            }
        }
        tracing::error!("注册用户失败: {}", e);
        return (StatusCode::INTERNAL_SERVER_ERROR, "注册失败").into_response();
    }

    let _ = tokio::fs::create_dir_all(format!("{}/{}", crate::constants::UPLOADS_DIR, payload.username)).await;

    let _ = log_audit(&pool, &payload.username, "register", None, None, extract_ip(&headers), extract_ua(&headers)).await;

    (StatusCode::OK, "用户初始化注册成功").into_response()
}

pub async fn logout(
    Extension(pool): Extension<sqlx::SqlitePool>,
    req: axum::http::Request<axum::body::Body>,
) -> impl IntoResponse {
    let headers = req.headers();
    let token_opt = headers
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|t| t.to_string());

    let username = if let Some(token) = &token_opt {
        let token_hash = hash_token(token);
        match sqlx::query("SELECT username FROM sessions WHERE token = ?")
            .bind(&token_hash)
            .fetch_optional(&pool)
            .await
        {
            Ok(Some(row)) => Some(row.get::<String, _>("username")),
            Ok(None) => {
                tracing::warn!("logout: token not found in sessions");
                None
            }
            Err(e) => {
                tracing::error!("logout: session query failed: {}", e);
                None
            }
        }
    } else {
        None
    };

    if let Some(token) = token_opt {
        let token_hash = hash_token(&token);
        let _ = sqlx::query("DELETE FROM sessions WHERE token = ?")
            .bind(token_hash)
            .execute(&pool)
            .await;
    }

    if let Some(name) = username {
        let _ = log_audit(&pool, &name, "logout", None, None, extract_ip(headers), extract_ua(headers)).await;
    }

    (StatusCode::OK, "已退出登录").into_response()
}