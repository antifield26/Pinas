// ====== WebDAV：Basic 认证（P1-1 拆分） ======
// 60s 成功缓存（键含凭证指纹）+ 对端 IP 限速 + 哑哈希等时（防用户枚举）
use axum::{
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

/// 已认证的 WebDAV 用户（提取器）
pub struct DavUser {
    pub username: String,
    pub is_admin: bool,
}

static AUTH_CACHE: LazyLock<Mutex<HashMap<String, (String, Instant)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"pi_nas\"")],
        "Unauthorized",
    )
        .into_response()
}

/// 密码变更后失效认证缓存（L8：历史实现改密/重置后旧凭证仍有 60s 的 Basic 访问窗口）
pub fn invalidate_dav_auth_cache(username: &str) {
    AUTH_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(username);
}

/// 手动 Basic 认证（WebDAV 单 handler 内调用）：成功 → DavUser；失败 → 401 响应。
/// 60s 成功缓存防每请求 argon2（键值含凭证指纹，见 AUTH_CACHE）；角色实时查（权限变更即时生效）。
/// 未命中缓存时先过限速：Argon2 ~100ms 是公网可用的 CPU DoS 面，必须按对端 IP 限频。
pub(crate) async fn dav_auth(
    headers: HeaderMap,
    peer_ip: Option<std::net::IpAddr>,
    pool: &SqlitePool,
) -> Result<DavUser, Response> {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(unauthorized)?;
    let (scheme, b64) = auth.split_once(' ').ok_or_else(unauthorized)?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return Err(unauthorized());
    }
    let creds = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or_else(unauthorized)?;
    let (user, pass) = creds.split_once(':').ok_or_else(unauthorized)?;

    // 命中缓存（60s）时跳过 argon2 验证；指纹必须与本次提交的凭证完全一致。角色实时查。
    // 锁不得跨越 await（MutexGuard 非 Send）
    // 中毒恢复：panic=abort 下 Mutex 中毒的 unwrap 会把整站打死，必须 into_inner 恢复
    let cred_fp = crate::core::hash_token(&format!("{user}\u{0}{pass}"));
    let cached_ok = {
        let cache = AUTH_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .get(user)
            .map(|(fp, t)| *fp == cred_fp && t.elapsed() < std::time::Duration::from_secs(60))
            .unwrap_or(false)
    };
    if cached_ok {
        let role: Option<String> = sqlx::query_scalar("SELECT role FROM users WHERE username = ?")
            .bind(user)
            .fetch_optional(pool)
            .await
            .unwrap_or(None);
        return Ok(DavUser {
            username: user.to_string(),
            is_admin: role.as_deref() == Some(crate::constants::ROLE_ADMIN),
        });
    }

    // 限速：仅未命中缓存（即必须跑 argon2）的尝试计数——成功路径 60s 内走缓存不再消耗额度。
    // 键与登录限速同源（回环信任 CF-Connecting-IP，直连用真实对端 IP）
    let rate_key = crate::handlers::auth::extract_ip(peer_ip, &headers)
        .unwrap_or_else(|| format!("dav:user:{user}"));
    if !crate::handlers::rate_limit::check_rate_limit(
        &rate_key,
        crate::constants::LOGIN_RATE_LIMIT_ATTEMPTS,
        std::time::Duration::from_secs(crate::constants::LOGIN_RATE_LIMIT_WINDOW_SECS),
    )
    .await
    {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            [(header::WWW_AUTHENTICATE, "Basic realm=\"pi_nas\"")],
            "请求过于频繁，请稍后再试",
        )
            .into_response());
    }

    // 用户不存在时用哑哈希等时校验（抹平用户枚举时序侧信道）
    let auth_row = match crate::db::queries::get_user_auth(pool, user).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            let dummy_pass = pass.to_string();
            let _ = tokio::task::spawn_blocking(move || {
                crate::core::verify_password(
                    crate::handlers::auth::dummy_hash_for_timing(),
                    &dummy_pass,
                )
            })
            .await
            .unwrap_or(false);
            return Err(unauthorized());
        }
        Err(_) => return Err(unauthorized()),
    };
    // Argon2 为阻塞 CPU 操作，必须移出 async 运行时（同 auth.rs 登录路径）
    let pass2 = pass.to_string();
    let hash2 = auth_row.0.clone();
    let ok = tokio::task::spawn_blocking(move || crate::core::verify_password(&hash2, &pass2))
        .await
        .unwrap_or(false);
    if !ok {
        return Err(unauthorized());
    }
    if auth_row.2 {
        return Err((
            StatusCode::FORBIDDEN,
            [(header::WWW_AUTHENTICATE, "Basic realm=\"pi_nas\"")],
            "请先通过网页登录修改初始密码",
        )
            .into_response());
    }
    AUTH_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(user.to_string(), (cred_fp, Instant::now()));
    Ok(DavUser {
        username: user.to_string(),
        is_admin: auth_row.1 == crate::constants::ROLE_ADMIN,
    })
}
