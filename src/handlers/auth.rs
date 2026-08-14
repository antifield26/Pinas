use argon2::{Argon2, PasswordHash, PasswordVerifier};
use axum::{
    extract::Extension,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::config::Config;
use crate::constants::*;
use crate::core::hash_token;
use crate::db::queries;
use crate::handlers::rate_limit;
use crate::handlers::utils::log_audit;

/// 未知用户登录/认证时用哑哈希做等时校验，抹平"用户是否存在"的时序侧信道
static DUMMY_HASH: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    crate::handlers::utils::hash_password("pinas-dummy-hash-constant-v1").unwrap_or_default()
});

/// 供 WebDAV 认证等路径复用的哑哈希（未知用户等时校验）
pub fn dummy_hash_for_timing() -> &'static str {
    &DUMMY_HASH
}

/// 对密码执行 Argon2 校验（阻塞 CPU 操作移入 spawn_blocking，避免卡住 async 运行时）
async fn verify_password_async(hash: &str, password: &str) -> bool {
    let hash = hash.to_string();
    let password = password.to_string();
    tokio::task::spawn_blocking(move || match PasswordHash::new(&hash) {
        Ok(parsed_hash) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok(),
        Err(e) => {
            tracing::error!("[Auth] 密码哈希解析失败: {}", e);
            false
        }
    })
    .await
    .unwrap_or(false)
}

/// 校验用户名格式：2-32 字符，仅允许字母/数字/下划线/连字符
fn validate_username(username: &str) -> Result<(), &'static str> {
    if username.len() < 2 || username.len() > 32 {
        return Err("用户名长度必须为 2-32 个字符");
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("用户名仅允许字母、数字、下划线和连字符");
    }
    Ok(())
}

/// 校验密码格式：6-128 字符
fn validate_password(password: &str) -> Result<(), &'static str> {
    if password.len() < 6 || password.len() > 128 {
        return Err("密码长度必须为 6-128 个字符");
    }
    Ok(())
}

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

/// 从请求中提取会话 token：优先 Cookie（浏览器场景），其次 Authorization Bearer（API 场景）
/// 与 crate::core::auth::auth_middleware 的提取优先级保持一致
fn extract_auth_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get("Cookie")
        .and_then(|h| h.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|c| {
                let c = c.trim();
                c.strip_prefix("auth_token=").map(|t| t.to_string())
            })
        })
        .or_else(|| {
            headers
                .get("Authorization")
                .and_then(|h| h.to_str().ok())
                .and_then(|h| h.strip_prefix("Bearer "))
                .map(|t| t.to_string())
        })
}

/// Cookie 是否带 Secure 标志：默认强制 Secure（部署于 CF 隧道后，公网 HTTPS）。
/// 仅纯 HTTP 局域网场景需显式配置 PINAS_COOKIE_SECURE=false
/// Cookie Domain 属性：统一登录（drive/dsh 同注册域共享会话）
fn cookie_domain_flag(config: &Config) -> String {
    match &config.cookie_domain {
        Some(d) => format!("; Domain={}", d),
        None => String::new(),
    }
}

fn should_secure_cookie(config: &Config) -> bool {
    config.cookie_secure.unwrap_or(true)
}

/// 从请求提取限速键（客户端 IP）。
/// - 直连(对端非回环):忽略一切客户端头,用真实对端 IP —— 杜绝伪造 X-Forwarded-For 绕过限速
/// - 回环(cloudflared 本地隧道):信任 CF-Connecting-IP > X-Real-IP > X-Forwarded-For 最左侧
/// - 无 ConnectInfo(测试场景):回退信任头,无头则 None(调用方按用户名限速)
pub fn extract_ip(peer_ip: Option<std::net::IpAddr>, headers: &HeaderMap) -> Option<String> {
    match peer_ip {
        Some(ip) if !ip.is_loopback() => return Some(ip.to_string()),
        Some(_) => {
            for name in ["cf-connecting-ip", "x-real-ip", "x-forwarded-for"] {
                if let Some(v) = headers.get(name).and_then(|v| v.to_str().ok()) {
                    let first = v.split(',').map(|s| s.trim()).find(|s| !s.is_empty());
                    if let Some(ip) = first {
                        return Some(ip.to_string());
                    }
                }
            }
            return Some("loopback".to_string());
        }
        None => {
            for name in ["x-real-ip", "x-forwarded-for"] {
                if let Some(v) = headers.get(name).and_then(|v| v.to_str().ok()) {
                    let first = v.split(',').map(|s| s.trim()).find(|s| !s.is_empty());
                    if let Some(ip) = first {
                        return Some(ip.to_string());
                    }
                }
            }
        }
    }
    None
}

/// 可选的客户端对端地址提取器：生产由 axum::serve 的 ConnectInfo 注入；
/// 测试直连(无扩展)时返回 None，回退按用户名限速，不影响测试。
pub struct MaybePeer(pub Option<std::net::IpAddr>);

impl<S> axum::extract::FromRequestParts<S> for MaybePeer
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        let ip = parts
            .extensions
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|c| c.0.ip());
        Ok(MaybePeer(ip))
    }
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
    MaybePeer(peer_ip): MaybePeer,
    headers: HeaderMap,
    Json(payload): Json<LoginRequest>,
) -> impl IntoResponse {
    // 仅校验用户名格式（密码校验仅在注册/修改密码时执行，登录不校验以兼容旧密码）
    if let Err(msg) = validate_username(&payload.username) {
        return (StatusCode::BAD_REQUEST, msg).into_response();
    }

    // 速率限制：每个客户端 IP 每分钟最多 N 次登录尝试；无法获取 IP 时回退为按用户名限速
    let rate_key =
        extract_ip(peer_ip, &headers).unwrap_or_else(|| format!("user:{}", payload.username));
    if !rate_limit::check_rate_limit(
        &rate_key,
        LOGIN_RATE_LIMIT_ATTEMPTS,
        std::time::Duration::from_secs(LOGIN_RATE_LIMIT_WINDOW_SECS),
    )
    .await
    {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "登录尝试过于频繁，请稍后再试",
        )
            .into_response();
    }

    let row = queries::get_user_auth(&pool, &payload.username)
        .await
        .unwrap_or(None);

    if let Some((db_hash, role, must_change)) = row {
        let is_valid = verify_password_async(&db_hash, &payload.password).await;

        if is_valid {
            // 登录成功即删除含明文密码的凭据文件（一次性引导文件）
            let _ = tokio::fs::remove_file("credentials.txt").await;
            let token = Uuid::new_v4().to_string();
            let token_hash = hash_token(&token);
            let expires = chrono::Utc::now()
                .checked_add_signed(chrono::Duration::days(config.session_days))
                .unwrap_or_else(|| chrono::Utc::now() + chrono::Duration::days(7))
                .format("%Y-%m-%d %H:%M:%S")
                .to_string();

            if let Err(e) =
                queries::create_session(&pool, &token_hash, &payload.username, &role, &expires)
                    .await
            {
                tracing::error!("创建会话失败: {}", e);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "创建会话失败，请稍后重试",
                )
                    .into_response();
            }

            let _ = log_audit(
                &pool,
                &payload.username,
                "login",
                None,
                None,
                extract_ip(peer_ip, &headers).as_deref(),
                extract_ua(&headers),
            )
            .await;

            // 构建 httpOnly Cookie（服务器端设置，JS 不可访问）
            // Secure 标志确保 Cookie 仅通过 HTTPS 传输（反向代理场景必需）
            let max_age = config.session_days * 86400;
            let secure_flag = if should_secure_cookie(&config) {
                "; Secure"
            } else {
                ""
            };
            let cookie = format!(
                "auth_token={}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}{}{}",
                token,
                max_age,
                cookie_domain_flag(&config),
                secure_flag
            );

            let mut resp = (
                StatusCode::OK,
                Json(LoginResponse {
                    token: token.clone(),
                    username: payload.username,
                    role,
                    must_change_pwd: must_change,
                }),
            )
                .into_response();
            resp.headers_mut().insert(
                axum::http::header::SET_COOKIE,
                cookie
                    .parse()
                    .unwrap_or_else(|_| HeaderValue::from_static("")),
            );
            return resp;
        }
        tracing::warn!("[Login] 用户 '{}' 密码验证失败", payload.username);
    } else {
        tracing::warn!("[Login] 用户 '{}' 不存在", payload.username);
        // 等时哑哈希校验：抹平用户枚举时序侧信道
        let _ = verify_password_async(&DUMMY_HASH, &payload.password).await;
    }
    (StatusCode::UNAUTHORIZED, "账号或访问密码校验失败").into_response()
}

#[tracing::instrument(skip(pool, config, headers, payload))]
pub async fn register(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(config): Extension<Config>,
    MaybePeer(peer_ip): MaybePeer,
    headers: HeaderMap,
    Json(payload): Json<RegisterRequest>,
) -> impl IntoResponse {
    use crate::handlers::utils::hash_password;

    // 注册开关（PINAS_ALLOW_REGISTRATION）——默认关闭
    if !config.allow_registration {
        return (StatusCode::FORBIDDEN, "注册未开放").into_response();
    }

    // 输入校验
    if let Err(msg) = validate_username(&payload.username) {
        return (StatusCode::BAD_REQUEST, msg).into_response();
    }
    if let Err(msg) = validate_password(&payload.password) {
        return (StatusCode::BAD_REQUEST, msg).into_response();
    }

    // 速率限制：每个客户端 IP 每小时最多 N 次注册；无法获取 IP 时回退为按用户名限速
    let rate_key =
        extract_ip(peer_ip, &headers).unwrap_or_else(|| format!("user:{}", payload.username));
    if !rate_limit::check_rate_limit(
        &rate_key,
        REGISTER_RATE_LIMIT_ATTEMPTS,
        std::time::Duration::from_secs(REGISTER_RATE_LIMIT_WINDOW_SECS),
    )
    .await
    {
        return (StatusCode::TOO_MANY_REQUESTS, "注册过于频繁，请稍后再试").into_response();
    }

    if queries::user_exists(&pool, &payload.username)
        .await
        .unwrap_or(false)
    {
        return (StatusCode::BAD_REQUEST, "用户已被注册").into_response();
    }

    let pwd = payload.password.clone();
    let hashed_password = match tokio::task::spawn_blocking(move || hash_password(&pwd)).await {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "服务内部错误").into_response(),
    };

    let count = queries::count_users(&pool).await.unwrap_or(0);
    let role = if count == 0 { ROLE_ADMIN } else { ROLE_USER };

    // 直接 INSERT，依赖 UNIQUE 约束防止 TOCTOU 竞态
    // quota_mb 显式写入默认配额（否则 NULL 被当作 0，注册用户将无法上传）
    let insert_result =
        sqlx::query("INSERT INTO users (username, password, role, quota_mb) VALUES (?, ?, ?, ?)")
            .bind(&payload.username)
            .bind(&hashed_password)
            .bind(role)
            .bind(config.default_quota_mb)
            .execute(&pool)
            .await;

    if let Err(e) = insert_result {
        if let Some(db_err) = e.as_database_error()
            && db_err.is_unique_violation()
        {
            return (StatusCode::CONFLICT, "用户已被注册").into_response();
        }
        tracing::error!("注册用户失败: {}", e);
        return (StatusCode::INTERNAL_SERVER_ERROR, "注册失败").into_response();
    }

    let _ = tokio::fs::create_dir_all(format!(
        "{}/{}",
        crate::constants::UPLOADS_DIR,
        payload.username
    ))
    .await;

    let _ = log_audit(
        &pool,
        &payload.username,
        "register",
        None,
        None,
        extract_ip(None, &headers).as_deref(),
        extract_ua(&headers),
    )
    .await;

    (StatusCode::OK, "用户初始化注册成功").into_response()
}

/// POST /api/user/password — 修改自己的密码（需已登录）
#[derive(Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

#[tracing::instrument(skip(pool, headers, payload))]
pub async fn change_password(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(config): Extension<Config>,
    headers: HeaderMap,
    Json(payload): Json<ChangePasswordRequest>,
) -> impl IntoResponse {
    use crate::handlers::utils::hash_password;

    // 输入校验
    if let Err(msg) = validate_password(&payload.new_password) {
        return (StatusCode::BAD_REQUEST, msg).into_response();
    }

    // 从 Cookie 或 Header 提取当前用户
    let current_token = match extract_auth_token(&headers) {
        Some(t) if !t.is_empty() => t,
        _ => return (StatusCode::UNAUTHORIZED, "未登录").into_response(),
    };

    // 从 session 获取 username
    let token_hash = hash_token(&current_token);
    let username: Option<String> = sqlx::query_scalar(
        "SELECT username FROM sessions WHERE token = ? AND expires_at > datetime('now')",
    )
    .bind(&token_hash)
    .fetch_optional(&pool)
    .await
    .unwrap_or(None);

    let username = match username {
        Some(u) => u,
        None => return (StatusCode::UNAUTHORIZED, "会话已过期，请重新登录").into_response(),
    };

    // 限速：每用户每分钟最多 3 次改密尝试（会话被盗后防 current_password 爆破）
    let rate_key = format!("pwd:{}", username);
    if !crate::handlers::rate_limit::check_rate_limit(
        &rate_key,
        3,
        std::time::Duration::from_secs(60),
    )
    .await
    {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "修改密码过于频繁，请稍后再试",
        )
            .into_response();
    }

    // 验证当前密码
    let row = queries::get_user_auth(&pool, &username)
        .await
        .unwrap_or(None);
    let db_hash = match row {
        Some((hash, _, _)) => hash,
        None => return (StatusCode::NOT_FOUND, "用户不存在").into_response(),
    };

    let current_pwd = payload.current_password.clone();
    let is_valid = tokio::task::spawn_blocking(move || match PasswordHash::new(&db_hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(current_pwd.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    })
    .await
    .unwrap_or(false);

    if !is_valid {
        return (StatusCode::BAD_REQUEST, "当前密码错误").into_response();
    }

    // 哈希新密码
    let new_pwd = payload.new_password.clone();
    let hashed = match tokio::task::spawn_blocking(move || hash_password(&new_pwd)).await {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "服务内部错误").into_response(),
    };

    // 事务：更新密码 + 清除 must_change_pwd + 清除所有旧会话
    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            tracing::error!("开启事务失败: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "服务内部错误").into_response();
        }
    };

    if let Err(e) = sqlx::query("DELETE FROM sessions WHERE username = ?")
        .bind(&username)
        .execute(&mut *tx)
        .await
    {
        tracing::error!("清除会话失败: {}", e);
        return (StatusCode::INTERNAL_SERVER_ERROR, "服务内部错误").into_response();
    }

    if let Err(e) =
        sqlx::query("UPDATE users SET password = ?, must_change_pwd = 0 WHERE username = ?")
            .bind(&hashed)
            .bind(&username)
            .execute(&mut *tx)
            .await
    {
        tracing::error!("更新密码失败: {}", e);
        return (StatusCode::INTERNAL_SERVER_ERROR, "服务内部错误").into_response();
    }

    if let Err(e) = tx.commit().await {
        tracing::error!("提交事务失败: {}", e);
        return (StatusCode::INTERNAL_SERVER_ERROR, "服务内部错误").into_response();
    }

    // 创建新会话并设置 cookie
    let new_token = Uuid::new_v4().to_string();
    let new_token_hash = hash_token(&new_token);
    let expires = chrono::Utc::now()
        .checked_add_signed(chrono::Duration::days(config.session_days))
        .unwrap_or_else(|| chrono::Utc::now() + chrono::Duration::days(7))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();

    let role = sqlx::query("SELECT role FROM users WHERE username = ?")
        .bind(&username)
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten()
        .map(|r| r.get::<String, _>("role"))
        .unwrap_or_else(|| "user".to_string());

    if let Err(e) =
        queries::create_session(&pool, &new_token_hash, &username, &role, &expires).await
    {
        tracing::error!("创建新会话失败: {}", e);
        return (StatusCode::INTERNAL_SERVER_ERROR, "服务内部错误").into_response();
    }

    let _ = log_audit(
        &pool,
        &username,
        "change_password",
        None,
        None,
        extract_ip(None, &headers).as_deref(),
        extract_ua(&headers),
    )
    .await;

    let max_age = config.session_days * 86400;
    let secure_flag = if should_secure_cookie(&config) {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!(
        "auth_token={}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}{}{}",
        new_token,
        max_age,
        cookie_domain_flag(&config),
        secure_flag
    );

    let mut resp = (StatusCode::OK, "密码修改成功").into_response();
    resp.headers_mut().insert(
        axum::http::header::SET_COOKIE,
        cookie
            .parse()
            .unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    resp
}

pub async fn logout(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(config): Extension<Config>,
    req: axum::http::Request<axum::body::Body>,
) -> impl IntoResponse {
    let headers = req.headers();
    // Cookie 优先（浏览器场景），Bearer 兜底（API 场景）— 与 auth_middleware 一致
    let token_opt = extract_auth_token(headers);

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
        let _ = queries::delete_session(&pool, &hash_token(&token)).await;
    }

    if let Some(name) = username {
        let _ = log_audit(
            &pool,
            &name,
            "logout",
            None,
            None,
            extract_ip(None, headers).as_deref(),
            extract_ua(headers),
        )
        .await;
    }

    let secure_flag = if should_secure_cookie(&config) {
        "; Secure"
    } else {
        ""
    };
    let mut resp = (StatusCode::OK, "已退出登录").into_response();
    resp.headers_mut().insert(
        axum::http::header::SET_COOKIE,
        format!(
            "auth_token=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0{}{}",
            cookie_domain_flag(&config),
            secure_flag
        )
        .parse()
        .unwrap(),
    );
    // 通知 HTMX 整页跳转到登录页
    resp.headers_mut()
        .insert("HX-Redirect", "/login".parse().unwrap());
    resp
}
