// ====== dsh 反代（DeepSeek Harness Web UI 统一登录入口） ======
// 架构：浏览器 → cloudflared (dsh.antifield.work) → 本服务第二监听 127.0.0.1:3100
//        → auth_middleware 会话校验 → 全量 HTTP/WS 转发 → dsh 127.0.0.1:3080
// 关键约束（dsh 信任栅栏 dsh-client-connection）：
//   - dsh 仅监听环回、拒绝 --host 0.0.0.0；本代理是唯一入口
//   - 上游请求的 Host 头必须等于公网主机名（否则 403）
//   - 浏览器端事件通道是 WebSocket（/api/events.mux、/api/events.host），须透传升级

use crate::config::Config;
use crate::core::auth::auth_middleware;
use axum::{
    body::Body,
    extract::{
        ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade},
        FromRequestParts, Request,
    },
    http::{
        header::{self, HeaderValue},
        HeaderMap, StatusCode,
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::any,
    Extension, Router,
};
use futures_util::{SinkExt, StreamExt, TryStreamExt};
use serde_json::json;
use std::{sync::LazyLock, time::Duration};
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tracing::{info, warn};

/// dsh 上游 HTTP 客户端：3s 连接超时，不设整体超时（长连接/WS 透传）
static DSH_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .build()
        .expect("Failed to build dsh proxy client")
});

/// hop-by-hop 头（RFC 7230 §6.1）：不得透传，由每个代理段自行处理
const HOP_BY_HOP: [&str; 8] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// 判断是否为 WebSocket 升级请求
fn is_ws_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONNECTION)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.to_ascii_lowercase()
                .split(',')
                .any(|p| p.trim() == "upgrade")
        })
        .unwrap_or(false)
        && headers
            .get(header::UPGRADE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_ascii_lowercase() == "websocket")
            .unwrap_or(false)
}

/// 入口：仅 admin 可用（Harness 是高级功能，UI 入口也仅对 admin 展示）；
/// WS 升级请求走 WebSocket 双向泵，其余全量 HTTP 转发
async fn dsh_entry(
    Extension(config): Extension<Config>,
    Extension(session): Extension<crate::core::UserSession>,
    req: Request<Body>,
) -> Response {
    if session.role != "admin" {
        warn!("[dsh] 非 admin 访问被拒: {}", session.username);
        return (
            StatusCode::FORBIDDEN,
            axum::Json(json!({ "error": "仅管理员可访问" })),
        )
            .into_response();
    }
    let (mut parts, body) = req.into_parts();

    if is_ws_upgrade(&parts.headers) {
        // 快照握手头（on_upgrade 闭包需要所有权；dsh 栅栏检查 Origin.host == Host.host）
        let origin = parts
            .headers
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let sec_fetch_site = parts
            .headers
            .get("sec-fetch-site")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let sec_fetch_mode = parts
            .headers
            .get("sec-fetch-mode")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        // 透传浏览器握手凭据（tungstenite 对已构造的 Request 不会自动补 Sec-WebSocket-*）
        let sec_ws_key = parts
            .headers
            .get("sec-websocket-key")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let sec_ws_version = parts
            .headers
            .get("sec-websocket-version")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let upstream_addr = upstream_addr(&config);
        let public_host = config.dsh_public_host.clone();

        match <WebSocketUpgrade as FromRequestParts<()>>::from_request_parts(&mut parts, &()).await {
            Ok(ws) => {
                let path = parts.uri.path().to_string();
                return ws
                    .on_upgrade(move |socket| {
                        dsh_ws_loop(
                            socket,
                            path,
                            upstream_addr,
                            public_host,
                            origin,
                            sec_fetch_site,
                            sec_fetch_mode,
                            sec_ws_key,
                            sec_ws_version,
                        )
                    })
                    .into_response();
            }
            Err(status) => {
                warn!("[dsh] WS 升级拒绝: {}", status);
                return status.into_response();
            }
        }
    }

    let req = Request::from_parts(parts, body);
    dsh_proxy(config, req).await
}

/// dsh 配置平面特权方法（dsh-client-connection PRIVILEGED_METHODS）：
/// trusted-host 只是 DNS-rebinding 围栏（非认证），配置平面被钉死在 loopback-same-origin。
/// 本代理是经 pinas admin 认证的受信中间层，转发时把 Host 指回环回并剥除 Origin
/// （Origin.host 必须 == Host.host 的环回检查），否则浏览器场景 settings/credentials 全部 403。
const PRIVILEGED_METHODS: [&str; 15] = [
    "agentPreset.read",
    "agentPreset.copy",
    "agentPreset.openDocument",
    "agentPreset.remove",
    "host.pickDirectory",
    "host.openPath",
    "settings.describe",
    "settings.openDocument",
    "settings.update",
    "settings.replace",
    "settings.mutate",
    "credentials.describe",
    "credentials.set",
    "credentials.unset",
    "llm.discoverModels",
];

fn is_privileged_api(path: &str) -> bool {
    let method = path.split('?').next().unwrap_or(path);
    if let Some(m) = method.strip_prefix("/api/") {
        PRIVILEGED_METHODS.contains(&m)
    } else {
        false
    }
}

/// 从 dsh_upstream_url 解析出 "host:port"
fn upstream_addr(config: &Config) -> String {
    config
        .dsh_upstream_url
        .trim_start_matches("http://")
        .trim_start_matches("ws://")
        .trim_end_matches('/')
        .to_string()
}

/// 全量 HTTP 转发：路径/查询/头透传，强制 Host 为公网主机名，响应体流式透传
async fn dsh_proxy(config: Config, req: Request<Body>) -> Response {
    let (parts, body) = req.into_parts();
    let method = parts.method.clone();
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    let upstream = format!(
        "{}{}",
        config.dsh_upstream_url.trim_end_matches('/'),
        path_and_query
    );

    let privileged = is_privileged_api(path_and_query);
    let mut builder = DSH_CLIENT.request(method, &upstream);
    for (name, value) in parts.headers.iter() {
        let name_lc = name.as_str().to_ascii_lowercase();
        // 特权方法：Origin 一并剥除（环回检查要求 Origin.host == Host.host，二者同为环回）
        if name_lc == "host"
            || name_lc == "content-length"
            || (privileged && name_lc == "origin")
            || HOP_BY_HOP.contains(&name_lc.as_str())
        {
            continue;
        }
        builder = builder.header(name, value);
    }
    // 信任栅栏：上游必须看到公网 Host 头（URL host 保持环回地址，Host 头单独注入）；
    // 特权方法（settings/credentials 等配置平面）只认环回 Host
    let host_value = if privileged {
        upstream_addr(&config)
    } else {
        config
            .dsh_public_host
            .clone()
            .unwrap_or_else(|| "127.0.0.1".to_string())
    };
    builder = builder.header(header::HOST, host_value);
    // 透传 body（流式，避免缓冲大文件）
    builder = builder.body(reqwest::Body::wrap_stream(
        body.into_data_stream()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)),
    ));

    match builder.send().await {
        Ok(res) => {
            let status = res.status();
            let mut rb = Response::builder().status(status);
            for (name, value) in res.headers().iter() {
                if HOP_BY_HOP.contains(&name.as_str().to_ascii_lowercase().as_str()) {
                    continue;
                }
                rb = rb.header(name, value);
            }
            match rb.body(Body::from_stream(res.bytes_stream())) {
                Ok(resp) => resp,
                Err(e) => {
                    warn!("[dsh] 响应组装失败: {}", e);
                    bad_gateway(&parts.headers)
                }
            }
        }
        Err(e) => {
            warn!("[dsh] 上游不可达 {}: {}", upstream, e);
            bad_gateway(&parts.headers)
        }
    }
}

/// 上游不可达的 502：导航请求给品牌页，API 请求给 JSON
fn bad_gateway(headers: &HeaderMap) -> Response {
    let wants_json = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|a| a.contains("application/json"))
        .unwrap_or(false);
    if wants_json {
        return (
            StatusCode::BAD_GATEWAY,
            axum::Json(json!({ "error": "harness 服务未运行" })),
        )
            .into_response();
    }
    (
        StatusCode::BAD_GATEWAY,
        axum::response::Html(
            r#"<!doctype html><html lang="zh"><head><meta charset="utf-8">
<title>Harness 未运行</title><style>
body{background:#0b0b0f;color:#e5e7eb;font-family:system-ui,sans-serif;display:grid;place-items:center;min-height:100vh;margin:0}
.card{background:#16161d;border:1px solid #2a2a35;border-radius:12px;padding:40px 48px;text-align:center}
h1{font-size:20px;margin:0 0 8px} p{color:#9ca3af;margin:0;font-size:14px}
</style></head><body><div class="card"><h1>Harness 服务未运行</h1>
<p>DeepSeek Harness 当前未启动，请稍后再试（systemctl --user status dsh）</p>
</div></body></html>"#,
        ),
    )
        .into_response()
}

/// WebSocket 双向泵：浏览器 ↔ dsh 上游，仅放行事件下行路径
async fn dsh_ws_loop(
    socket: WebSocket,
    path: String,
    upstream_addr: String,
    public_host: Option<String>,
    origin: Option<String>,
    sec_fetch_site: Option<String>,
    sec_fetch_mode: Option<String>,
    sec_ws_key: Option<String>,
    sec_ws_version: Option<String>,
) {
    // 信任面最小化：只允许事件下行通道，其余 WS 路径一律拒绝
    if path != "/api/events.mux" && path != "/api/events.host" {
        warn!("[dsh] WS 路径被拒: {}", path);
        return;
    }

    // 上游握手：Host 必须为公网名（栅栏），Origin/sec-fetch 透传（Origin.host == Host.host 校验）
    let upstream_url = format!("ws://{}{}", upstream_addr, path);
    let mut handshake = axum::http::Request::builder().method("GET").uri(&upstream_url);
    if let Some(h) = &public_host {
        handshake = handshake.header(header::HOST, h);
    }
    if let Some(o) = &origin {
        handshake = handshake.header(header::ORIGIN, o);
    }
    if let Some(s) = &sec_fetch_site {
        handshake = handshake.header("sec-fetch-site", s);
    }
    if let Some(m) = &sec_fetch_mode {
        handshake = handshake.header("sec-fetch-mode", m);
    }
    // tungstenite 对已构造的 Request 不做任何自动补全：升级头 + 浏览器原始 Key/Version 都要
    handshake = handshake
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "websocket");
    if let Some(k) = &sec_ws_key {
        handshake = handshake.header("sec-websocket-key", k);
    }
    if let Some(v) = &sec_ws_version {
        handshake = handshake.header("sec-websocket-version", v);
    }
    let Ok(handshake) = handshake.body(()) else {
        warn!("[dsh] WS 握手构造失败");
        return;
    };

    let tcp = match tokio::net::TcpStream::connect(&upstream_addr).await {
        Ok(t) => t,
        Err(e) => {
            warn!("[dsh] WS 上游连接失败 {}: {}", upstream_addr, e);
            return;
        }
    };
    let (ws_up, _resp) = match tokio_tungstenite::client_async_with_config(handshake, tcp, None).await {
        Ok(v) => v,
        Err(e) => {
            warn!("[dsh] WS 上游握手失败: {}", e);
            return;
        }
    };
    info!("[dsh] WS 通道已建立: {}", path);

    let (mut axum_tx, mut axum_rx) = socket.split();
    let (up_tx, mut up_rx) = ws_up.split();

    // 浏览器 → 上游
    let mut fwd_tx = up_tx;
    tokio::spawn(async move {
        while let Some(Ok(msg)) = axum_rx.next().await {
            let Some(up_msg) = axum_to_ws(msg) else { break };
            let is_close = matches!(up_msg, WsMessage::Close(_));
            if fwd_tx.send(up_msg).await.is_err() || is_close {
                break;
            }
        }
    });

    // 上游 → 浏览器（主任务收尾）
    while let Some(Ok(msg)) = up_rx.next().await {
        let Some(axum_msg) = ws_to_axum(msg) else { break };
        let is_close = matches!(axum_msg, AxumMessage::Close(_));
        if axum_tx.send(axum_msg).await.is_err() || is_close {
            break;
        }
    }
}

/// axum 消息 → tungstenite 消息（字节级转换，规避两库类型版本差异）
fn axum_to_ws(m: AxumMessage) -> Option<WsMessage> {
    Some(match m {
        AxumMessage::Text(t) => WsMessage::Text(t.to_string().into()),
        AxumMessage::Binary(b) => WsMessage::Binary(b),
        AxumMessage::Ping(p) => WsMessage::Ping(p),
        AxumMessage::Pong(p) => WsMessage::Pong(p),
        AxumMessage::Close(c) => WsMessage::Close(c.map(|c| tokio_tungstenite::tungstenite::protocol::CloseFrame {
            code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::from(c.code),
            reason: c.reason.to_string().into(),
        })),
    })
}

/// tungstenite 消息 → axum 消息
fn ws_to_axum(m: WsMessage) -> Option<AxumMessage> {
    Some(match m {
        WsMessage::Text(t) => AxumMessage::Text(t.to_string().into()),
        WsMessage::Binary(b) => AxumMessage::Binary(b),
        WsMessage::Ping(p) => AxumMessage::Ping(p),
        WsMessage::Pong(p) => AxumMessage::Pong(p),
        WsMessage::Close(c) => AxumMessage::Close(c.map(|c| axum::extract::ws::CloseFrame {
            code: c.code.into(),
            reason: c.reason.to_string().into(),
        })),
        WsMessage::Frame(_) => return None,
    })
}

/// 认证门禁（置于 auth_middleware 外层）：
/// - 302 /login 重写为 drive 的绝对登录地址（带 redirect 回 dsh）
/// - 302 /change-password 同样指向 drive
/// - 401/403 且请求方要 JSON → 统一 401 JSON
pub async fn dsh_auth_gate(
    Extension(config): Extension<Config>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let uri = req.uri().clone();
    let accept_json = req
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|a| a.contains("application/json"))
        .unwrap_or(false);

    let mut resp = next.run(req).await;
    let status = resp.status();

    match status {
        // auth_middleware 的 Redirect::to() 默认 303 See Other
        StatusCode::SEE_OTHER | StatusCode::FOUND => {
            let loc = resp
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            if let (Some(drive), Some(public_host), Some(loc)) =
                (&config.drive_public_url, &config.dsh_public_host, loc)
            {
                let drive = drive.trim_end_matches('/');
                let new_loc = if loc.starts_with("/login") {
                    format!("{}/login?redirect=https://{}{}", drive, public_host, uri)
                } else if loc.starts_with("/change-password") {
                    format!("{}/change-password", drive)
                } else {
                    loc
                };
                if let Ok(v) = HeaderValue::from_str(&new_loc) {
                    resp.headers_mut().insert(header::LOCATION, v);
                }
            }
        }
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            if accept_json {
                return Ok((
                    StatusCode::UNAUTHORIZED,
                    axum::Json(json!({ "error": "unauthorized" })),
                )
                    .into_response());
            }
        }
        _ => {}
    }
    Ok(resp)
}

/// 构建 dsh 反代 Router（第二监听专用，不挂 pinas 的 CSP/压缩层）
pub fn build_dsh_router(config: Config, pool: sqlx::SqlitePool) -> Router {
    Router::new()
        .route("/{*path}", any(dsh_entry))
        .route("/", any(dsh_entry))
        // 后加的 layer 先执行：gate(外层) → auth_middleware(内层) → handler
        .layer(middleware::from_fn(auth_middleware))
        .layer(middleware::from_fn(dsh_auth_gate))
        .layer(Extension(pool))
        .layer(Extension(config))
        .layer(
            tower_http::set_header::SetResponseHeaderLayer::overriding(
                header::X_FRAME_OPTIONS,
                HeaderValue::from_static("DENY"),
            ),
        )
}
