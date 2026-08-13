use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue, header},
    middleware::Next,
    response::Response,
};

/// 安全响应头中间件：为所有响应添加安全相关的 HTTP 头
pub async fn security_headers(req: Request, next: Next) -> Response {
    // HSTS 只在 HTTPS 接入时下发：无条件下发会让纯 HTTP 局域网访问被浏览器
    // 永久强制升级（max-age 一年），本地直连立即不可用（需在 next.run 移走 req 前读取）
    let is_https = req
        .headers()
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("https"))
        .unwrap_or(false);
    let mut response = next.run(req).await;
    let headers = response.headers_mut();

    // Content-Security-Policy
    // 注：保留 'unsafe-inline'(htmx 片段内联脚本/内联处理器必需) 与 'unsafe-eval'(Alpine 表达式编译必需)，
    // 均为当前架构的硬依赖；XSS 防线以消除注入点 + Askama 转义为主。nonce 迁移列为后续改造。
    // 已移除死条目：unpkg(资源已本地化)、static.cloudflareinsights.com(未使用)、ws:/wss:(无 WebSocket 路由)。
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; \
             script-src 'self' 'unsafe-inline' 'unsafe-eval'; \
             style-src 'self' 'unsafe-inline'; \
             img-src 'self' data: blob:; \
             media-src 'self' blob:; \
             font-src 'self'; \
             connect-src 'self' blob:; \
             frame-src 'self'; \
             frame-ancestors 'self'; \
             object-src 'none'; \
             base-uri 'self'; \
             form-action 'self'; \
             worker-src 'self'; \
             manifest-src 'self'",
        ),
    );

    // HSTS (仅 HTTPS 接入时生效；见 is_https 说明)
    if is_https {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        );
    }

    // 禁止 MIME 类型嗅探
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );

    // 禁止被嵌入 iframe
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));

    // Referrer 策略
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );

    // Permissions-Policy: 禁用不必要的浏览器特性
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );

    // Cross-Origin-Opener-Policy
    headers.insert(
        HeaderName::from_static("cross-origin-opener-policy"),
        HeaderValue::from_static("same-origin"),
    );

    response
}
