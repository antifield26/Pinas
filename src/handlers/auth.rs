use axum::{
    extract::Extension,
    http::StatusCode,
    response::{IntoResponse, Json},
};
use argon2::{Argon2, PasswordHash, PasswordVerifier};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;
use chrono;

use crate::handlers::utils::{hash_token, log_audit};
use crate::config::Config;

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
}

pub async fn login(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(config): Extension<Config>,
    Json(payload): Json<LoginRequest>,
) -> impl IntoResponse {
    let row = sqlx::query("SELECT password, role FROM users WHERE username = ?")
        .bind(&payload.username)
        .fetch_optional(&pool)
        .await
        .unwrap_or(None);

    if let Some(r) = row {
        let db_hash: String = r.get("password");
        let role: String = r.get("role");
        
        let parsed_hash = match PasswordHash::new(&db_hash) {
            Ok(h) => h,
            Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "凭证分析损坏").into_response(),
        };

        if Argon2::default().verify_password(payload.password.as_bytes(), &parsed_hash).is_ok() {
            let token = Uuid::new_v4().to_string();
            let token_hash = hash_token(&token);
            let expires = chrono::Utc::now()
                .checked_add_signed(chrono::Duration::days(config.session_days))
                .unwrap()
                .format("%Y-%m-%d %H:%M:%S")
                .to_string();

            let _ = sqlx::query("INSERT INTO sessions (token, username, role, expires_at) VALUES (?, ?, ?, ?)")
                .bind(&token_hash)
                .bind(&payload.username)
                .bind(&role)
                .bind(&expires)
                .execute(&pool)
                .await;

            let _ = log_audit(&pool, &payload.username, "login", None, None).await;

            return (StatusCode::OK, Json(LoginResponse {
                token,
                username: payload.username,
                role,
            })).into_response();
        }
    }
    (StatusCode::UNAUTHORIZED, "账号或访问密码校验失败").into_response()
}

pub async fn register(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Json(payload): Json<RegisterRequest>,
) -> impl IntoResponse {
    use argon2::password_hash::{rand_core::OsRng, PasswordHasher, SaltString};
    use argon2::Argon2;

    let user_exists = sqlx::query("SELECT username FROM users WHERE username = ?")
        .bind(&payload.username)
        .fetch_optional(&pool)
        .await
        .unwrap_or(None)
        .is_some();

    if user_exists {
        return (StatusCode::BAD_REQUEST, "用户已被注册").into_response();
    }

    let salt = SaltString::generate(&mut OsRng);
    let hashed_password = match Argon2::default().hash_password(payload.password.as_bytes(), &salt) {
        Ok(h) => h.to_string(),
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "加密组件损坏").into_response(),
    };

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap_or(0);
    let role = if count == 0 { "admin" } else { "user" };

    let _ = sqlx::query("INSERT INTO users (username, password, role) VALUES (?, ?, ?)")
        .bind(&payload.username)
        .bind(&hashed_password)
        .bind(role)
        .execute(&pool)
        .await;

    let _ = tokio::fs::create_dir_all(format!("uploads/{}", payload.username)).await;

    let _ = log_audit(&pool, &payload.username, "register", None, None).await;

    (StatusCode::OK, "用户初始化注册成功").into_response()
}

pub async fn logout(
    Extension(pool): Extension<sqlx::SqlitePool>,
    req: axum::http::Request<axum::body::Body>,
) -> impl IntoResponse {
    let token_opt = req.headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|t| t.to_string());

    let username = if let Some(token) = &token_opt {
        let token_hash = hash_token(token);
        sqlx::query("SELECT username FROM sessions WHERE token = ?")
            .bind(&token_hash)
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten()
            .map(|r| r.get::<String, _>("username"))
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
        let _ = log_audit(&pool, &name, "logout", None, None).await;
    }

    (StatusCode::OK, "已退出登录").into_response()
}