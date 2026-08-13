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

pub async fn auth_middleware(
    Extension(pool): Extension<sqlx::SqlitePool>,
    mut req: Request,
    next: Next,
) -> Result<impl IntoResponse, StatusCode> {
    let uri_path = req.uri().path();

    if uri_path.starts_with("/s/") {
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
    if target_token.is_none()
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
                let media_path = uri_path
                    .strip_prefix("/api/media/")
                    .unwrap_or_default()
                    .trim_start_matches('/');
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
    // role 实时取 users 表（不信任 sessions 快照）：admin 降权后未过期会话立即生效
    let session_row = sqlx::query(
        "SELECT s.username, COALESCE(u.role, s.role) as role, COALESCE(u.must_change_pwd, 0) as must_change_pwd \
         FROM sessions s LEFT JOIN users u ON s.username = u.username \
         WHERE s.token = ? AND s.expires_at > datetime('now')",
    )
    .bind(&token_hash)
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

    let (username, role, must_change_pwd) = match session_row {
        Some(row) => (
            row.get::<String, _>("username"),
            row.get::<String, _>("role"),
            row.get::<i64, _>("must_change_pwd") != 0,
        ),
        None => {
            warn!(
                "[Auth] Token 无效或已过期: {}...",
                &target_token.chars().take(8).collect::<String>()
            );
            return reject_no_token(uri_path);
        }
    };

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
