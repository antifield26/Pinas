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
    // script-src（H8 收敛）：'unsafe-inline' 已移除——全部业务脚本外置于 /assets/app.js，
    // 内联事件处理器全部 data-* 委托；仅两处 head 主题预涂脚本（防闪白必需）内联，
    // 经 sha256 哈希放行（哈希 = 脚本 innerText 逐字节精确值，模板改动需同步更新并跑回归测试）。
    //   第一哈希 e9AA… → templates/base.html 主题预涂脚本
    //   第二哈希 V1ON… → templates/partials/theme_head.html 独立页暗色预涂脚本
    // 'unsafe-eval' 保留：Alpine x-data 表达式编译与 htmx hx-on 求值依赖。
    // style-src 'unsafe-inline' 保留：动画延迟/进度条宽度等内联样式依赖。
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; \
             script-src 'self' 'unsafe-eval' \
               'sha256-e9AA0UHheOX1XAtrSv68GkyqcYbpzWeof+Mi19hqhkE=' \
               'sha256-V1ONiGmI3S/fo/iVTzDdp1MvwZTNDqZJWdZu1ok6Ees='; \
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
