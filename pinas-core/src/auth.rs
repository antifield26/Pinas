use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::IntoResponse,
    Extension,
};
use sqlx::Row;
use crate::UserSession;
use sha2::{Sha256, Digest};
use tracing::{warn, error};

fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
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

    let is_download = uri_path.starts_with("/downloads/");

    // 获取 token：media 路径支持 query 参数，其他路径仅支持 Header
    let target_token = if uri_path.starts_with("/api/media/") {
        // 从查询参数中获取 token
        req.uri().query()
            .and_then(|q| {
                for pair in q.split('&') {
                    let mut parts = pair.splitn(2, '=');
                    if parts.next() == Some("token") {
                        return parts.next().map(|v| v.to_string());
                    }
                }
                None
            })
            .or_else(|| {
                // 降级：尝试从 Authorization 头获取
                req.headers()
                    .get("Authorization")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|h| h.strip_prefix("Bearer "))
                    .map(|t| t.to_string())
            })
    } else {
        req.headers()
            .get("Authorization")
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "))
            .map(|t| t.to_string())
    };

    let target_token = match target_token {
        Some(token) if !token.is_empty() => token,
        _ => {
            warn!("[Auth] 未提供有效 Token, path={}", uri_path);
            return Err(StatusCode::UNAUTHORIZED);
        }
    };

    let token_hash = hash_token(&target_token);
    let session_row = sqlx::query(
        "SELECT username, role FROM sessions WHERE token = ? AND expires_at > datetime('now')"
    )
    .bind(&token_hash)
    .fetch_optional(&pool)
    .await
    .map_err(|e| {
        error!("[Auth Error] 数据库查询失败, token_prefix={}: {}", &target_token.chars().take(8).collect::<String>(), e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let (username, role) = match session_row {
        Some(row) => (row.get::<String, _>("username"), row.get::<String, _>("role")),
        None => {
            warn!("[Auth] Token 无效或已过期: {}...", &target_token.chars().take(8).collect::<String>());
            return Err(StatusCode::UNAUTHORIZED);
        }
    };

    // 下载权限检查保持不变...
    if is_download {
        let remaining_encoded = &uri_path["/downloads/".len()..]; 
        let remaining_path = urlencoding::decode(remaining_encoded)
            .map(|c| c.into_owned())
            .unwrap_or_else(|_| remaining_encoded.to_string());

        if role != "admin" {
            let user_prefix = format!("{}/", username);
            if !remaining_path.starts_with(&user_prefix) && remaining_path != username {
                warn!("[Auth Error] 普通用户越权下载: user={}, path={}", username, remaining_path);
                return Err(StatusCode::FORBIDDEN);
            }
        }
    }

    req.extensions_mut().insert(UserSession { username, role });
    Ok(next.run(req).await)
}