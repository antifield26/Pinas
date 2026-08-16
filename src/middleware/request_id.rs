// ====== X-Request-Id 中间件（P1-2：全链路排障贯穿） ======
// 每个请求生成/透传唯一 ID：
//   - 响应统一携带 X-Request-Id 头（配合 dsh 反代排查问题链路）
//   - tracing span 注入 request_id 字段——所有 handler 日志自动带上请求 ID
// 若客户端（如 dsh 反代/监控）已带 X-Request-Id 则沿用，否则生成 UUID v4。

use axum::{extract::Request, http::HeaderValue, middleware::Next, response::Response};
use tracing::Instrument;
use tracing::info_span;

pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// 请求扩展中的请求 ID（dsh 反代等下游转发可读取并注入上游请求）
#[derive(Clone, Debug)]
pub struct RequestId(pub String);

pub async fn request_id_middleware(mut req: Request, next: Next) -> Response {
    // 沿用入站 ID（dsh 反代等上游链路可关联）
    let incoming = req
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let id = incoming.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    req.extensions_mut().insert(RequestId(id.clone()));

    let span = info_span!(
        "http_request",
        request_id = %id,
        method = %req.method().as_str(),
        uri = %req.uri().path()
    );
    let mut resp = async { next.run(req).await }.instrument(span).await;
    if let Ok(hv) = HeaderValue::from_str(&id) {
        resp.headers_mut().insert(REQUEST_ID_HEADER, hv);
    }
    resp
}
