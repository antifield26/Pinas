use crate::core::UserSession;
use crate::core::crypto::hash_token;
use axum::{
    Extension,
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};
use sqlx::Row;
use tracing::{error, warn};

fn reject_no_token(uri_path: &str) -> Result<Response, StatusCode> {
    warn!("[Auth] 未提供有效 Token, path={}", uri_path);
    if uri_path.starts_with("/api/") || uri_path.starts_with("/s/") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let login_url = if uri_path == "/" || uri_path.is_empty() {
        "/login".to_string()
    } else {
        format!("/login?redirect={}", uri_path)
    };
    Ok(Redirect::to(&login_url).into_response())
}

/// 认证策略（P1-9 修复）：中间件的豁免能力按路由集显式声明，而非在中间件内
/// 写死路径前缀——历史实现把 `/s/` 豁免写死在 auth_middleware 内，导致 dsh 反代
/// 路由（`/{*path}` 全匹配）上的未认证 `/s/*` 请求被放行后因缺少 UserSession
/// 扩展而 500（可演变为认证绕过）。现在：
///   - 主路由：share_paths_public=true（/s/ 分享页走公开路由，中间件不拦截）、
///     allow_media_token=true（媒体播放走短时效路径令牌）
///   - dsh 路由：全 false——严格 admin 会话门禁，任何路径都不豁免
#[derive(Clone, Copy, Debug)]
pub struct AuthPolicy {
    pub share_paths_public: bool,
    pub allow_media_token: bool,
}

impl Default for AuthPolicy {
    fn default() -> Self {
        Self {
            share_paths_public: true,
            allow_media_token: true,
        }
    }
}

pub async fn auth_middleware(
    axum::extract::State(policy): axum::extract::State<AuthPolicy>,
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(config): Extension<crate::config::Config>,
    mut req: Request,
    next: Next,
) -> Result<impl IntoResponse, StatusCode> {
    let uri_path = req.uri().path();

    if policy.share_paths_public && uri_path.starts_with("/s/") {
        return Ok(next.run(req).await);
    }

    // 提取 token 的辅助闭包
    let extract_from_cookie = |req: &Request| -> Option<String> {
        req.headers()
            .get("Cookie")
            .and_then(|h| h.to_str().ok())
            .and_then(|cookies| {
                cookies.split(';').find_map(|c| {
                    let c = c.trim();
                    c.strip_prefix("auth_token=").map(|t| t.to_string())
                })
            })
    };
    let extract_from_header = |req: &Request| -> Option<String> {
        req.headers()
            .get("Authorization")
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "))
            .map(|t| t.to_string())
    };
    let extract_query_param = |req: &Request, name: &str| -> Option<String> {
        req.uri().query().and_then(|q| {
            for pair in q.split('&') {
                let mut parts = pair.splitn(2, '=');
                if parts.next() == Some(name) {
                    return parts.next().map(|v| v.to_string());
                }
            }
            None
        })
    };

    // 优先级：Cookie (httpOnly) > Authorization Header
    let target_token = extract_from_cookie(&req).or_else(|| extract_from_header(&req));

    // /api/media/* 无 Cookie/Header 时走短时效路径限定媒体令牌（mt）。
    // 会话 token 不再接受 URL 查询串（完整会话凭证进日志/历史的风险已被媒体令牌取代）
    if policy.allow_media_token
        && target_token.is_none()
        && uri_path.starts_with("/api/media/")
        && let Some(mt) = extract_query_param(&req, "mt")
        && !mt.is_empty()
    {
        let mt_hash = hash_token(&mt);
        let row = sqlx::query(
            "SELECT m.username, m.path_prefix, COALESCE(u.role, 'user') as role \
                 FROM media_tokens m LEFT JOIN users u ON m.username = u.username \
                 WHERE m.token_hash = ? AND m.expires_at > datetime('now')",
        )
        .bind(&mt_hash)
        .fetch_optional(&pool)
        .await
        .map_err(|e| {
            error!("[Auth Error] 媒体令牌查询失败: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
        match row {
            Some(row) => {
                // 路径限定：令牌只能访问其签发路径前缀之下的资源（边界感知，防 "dir" 匹配 "dir2"）
                let path_prefix: String = row.get("path_prefix");
                // P2-1：按解码后的路径逐段比较——uri.path() 是原始百分号编码，
                // 而 media_proxy 经 axum Path<String> 拿到解码后的路径；两者不一致
                // 会造成令牌作用域判定歧义。解码后比较与 handler 语义对齐
                // （解码出现的 `..` 由 handler 的 safe_join_sandbox + openat2 沙箱拒绝，fail-closed）。
                let media_path = pct_decode(
                    uri_path
                        .strip_prefix("/api/media/")
                        .unwrap_or_default()
                        .trim_start_matches('/'),
                );
                let prefix = path_prefix.trim_start_matches('/').trim_end_matches('/');
                let within = if prefix.is_empty() {
                    true
                } else {
                    media_path == prefix || media_path.starts_with(&format!("{}/", prefix))
                };
                if !within {
                    warn!("[Auth] 媒体令牌路径越界: {:?}", media_path);
                    return reject_no_token(uri_path);
                }
                req.extensions_mut().insert(UserSession {
                    username: row.get("username"),
                    role: row.get("role"),
                    must_change_pwd: false,
                });
                return Ok(next.run(req).await);
            }
            None => {
                warn!("[Auth] 媒体令牌无效或已过期");
                return reject_no_token(uri_path);
            }
        }
    }

    let target_token = match target_token {
        Some(token) if !token.is_empty() => token,
        _ => return reject_no_token(uri_path),
    };

    let token_hash = hash_token(&target_token);
    // 空闲超时（P0-2）：会话必须同时满足 未超过绝对过期 且 空闲未超时
    // （last_active_at 为 NULL = 升级前的旧会话，宽限为活跃，首次请求即刷新）
    let idle_mod = format!("-{} minutes", config.session_idle_minutes.max(1));
    // role 实时取 users 表（不信任 sessions 快照）：admin 降权后未过期会话立即生效
    let session_row = sqlx::query(
        "SELECT s.username, COALESCE(u.role, s.role) as role, COALESCE(u.must_change_pwd, 0) as must_change_pwd, s.last_active_at \
         FROM sessions s LEFT JOIN users u ON s.username = u.username \
         WHERE s.token = ? AND s.expires_at > datetime('now') \
           AND (s.last_active_at IS NULL OR s.last_active_at >= datetime('now', ?))",
    )
    .bind(&token_hash)
    .bind(&idle_mod)
    .fetch_optional(&pool)
    .await
    .map_err(|e| {
        error!(
            "[Auth Error] 数据库查询失败, token_prefix={}: {}",
            &target_token.chars().take(8).collect::<String>(),
            e
        );
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let (username, role, must_change_pwd, last_active_at) = match session_row {
        Some(row) => (
            row.get::<String, _>("username"),
            row.get::<String, _>("role"),
            row.get::<i64, _>("must_change_pwd") != 0,
            row.get::<Option<String>, _>("last_active_at"),
        ),
        None => {
            warn!(
                "[Auth] Token 无效或已过期: {}...",
                &target_token.chars().take(8).collect::<String>()
            );
            return reject_no_token(uri_path);
        }
    };

    // 滑动活跃时间（惰性刷新）：仅当超过 5 分钟未刷新时才写库，避免每请求一次写放大
    if last_active_at.as_deref().is_none_or(|v| v.is_empty()) {
        let _ = sqlx::query("UPDATE sessions SET last_active_at = datetime('now') WHERE token = ?")
            .bind(&token_hash)
            .execute(&pool)
            .await;
    } else {
        let _ = sqlx::query(
            "UPDATE sessions SET last_active_at = datetime('now') WHERE token = ? AND last_active_at < datetime('now', '-5 minutes')",
        )
        .bind(&token_hash)
        .execute(&pool)
        .await;
    }

    // 强制密码修改：must_change_pwd 时仅允许密码修改相关路由
    if must_change_pwd {
        let exempt = [
            "/change-password",
            "/api/user/password",
            "/api/logout",
            "/api/login",
        ];
        if !exempt.iter().any(|p| uri_path.starts_with(p)) {
            warn!(
                "[Auth] 用户 '{}' 需要修改密码，拒绝访问: {}",
                username, uri_path
            );
            if uri_path.starts_with("/api/") {
                return Err(StatusCode::FORBIDDEN);
            }
            return Ok(Redirect::to("/change-password").into_response());
        }
    }

    req.extensions_mut().insert(UserSession {
        username,
        role,
        must_change_pwd,
    });
    Ok(next.run(req).await)
}

/// 最小 percent-decode（RFC 3986）：仅解码 `%XX` 序列，其余字节原样保留。
/// 用于把原始 uri.path() 对齐到 axum Path 提取器解码后的语义（P2-1）。
/// 非法/截断的 % 序列按字面保留（fail-closed：不会放宽路径限定）。
fn pct_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
