// ====== SSH WebSocket 终端 ======
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Extension, Query,
    },
    response::IntoResponse,
};
use serde::Deserialize;
use std::{
    io::{Read, Write},
    time::Instant,
};
use tokio::sync::Mutex;

use pinas_core::UserSession;

#[derive(Debug, Deserialize)]
pub struct SshConnectParams {
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub username: String,
    // 密码不再通过 URL 参数传递，改为 WebSocket 连接后首条消息发送
}

fn default_ssh_port() -> u16 { 22 }

static ACTIVE_SESSIONS: std::sync::LazyLock<Mutex<std::collections::HashMap<String, Instant>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

pub async fn ssh_ws_handler(
    Extension(session): Extension<UserSession>,
    Query(params): Query<SshConnectParams>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let username = session.username.clone();

    // 并发控制
    {
        let mut sessions = ACTIVE_SESSIONS.lock().await;
        if let Some(since) = sessions.get(&username) {
            let elapsed = since.elapsed().as_secs();
            if elapsed < 600 {
                return (
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    format!("已有活跃 SSH 会话 ({}s 前建立)，请先断开", elapsed),
                ).into_response();
            }
            sessions.remove(&username);
        }
        sessions.insert(username.clone(), Instant::now());
    }

    ws.on_upgrade(move |socket| handle_socket(socket, params, username))
}

async fn handle_socket(mut ws: WebSocket, params: SshConnectParams, username: String) {
    // 1. 等待客户端通过首条 WebSocket 消息发送密码（不在 URL 中传递）
    let password = loop {
        match ws.recv().await {
            Some(Ok(Message::Text(text))) => {
                // 可能是 JSON 格式的认证信息
                if let Ok(auth) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(pwd) = auth.get("password").and_then(|v| v.as_str()) {
                        break pwd.to_string();
                    }
                }
                // 纯文本 = 直接作为密码
                if !text.starts_with('{') {
                    break text;
                }
                // JSON 但没有 password 字段，继续等
            }
            Some(Ok(Message::Close(_))) | None => {
                let _ = ws.send(Message::Text("\r\n❌ 连接在认证前关闭\r\n".into())).await;
                cleanup(&username).await;
                return;
            }
            _ => continue,
        }
    };

    tracing::info!("[SSH] {} 正在连接 {}@{}:{}", username, params.username, params.host, params.port);

    // 2. TCP 连接
    let addr = format!("{}:{}", params.host, params.port);
    let tcp = match tokio::task::spawn_blocking(move || std::net::TcpStream::connect(&addr)).await {
        Ok(Ok(s)) => s,
        _ => { let _ = ws.send(Message::Text("\r\n❌ TCP 连接失败\r\n".into())).await; cleanup(&username).await; return; }
    };

    let mut sess = match ssh2::Session::new() {
        Ok(s) => s,
        _ => { let _ = ws.send(Message::Text("\r\n❌ 创建 SSH 会话失败\r\n".into())).await; cleanup(&username).await; return; }
    };
    sess.set_tcp_stream(tcp);

    if let Err(e) = sess.handshake() {
        let _ = ws.send(Message::Text(format!("\r\n❌ SSH 握手失败: {}\r\n", e))).await;
        cleanup(&username).await; return;
    }

    if let Err(e) = sess.userauth_password(&params.username, &password) {
        let _ = ws.send(Message::Text(format!("\r\n❌ 认证失败: {}\r\n", e))).await;
        cleanup(&username).await; return;
    }

    if !sess.authenticated() {
        let _ = ws.send(Message::Text("\r\n❌ 认证被拒绝\r\n".into())).await;
        cleanup(&username).await; return;
    }

    let mut channel = match sess.channel_session() {
        Ok(c) => c,
        Err(e) => { let _ = ws.send(Message::Text(format!("\r\n❌ 打开通道失败: {}\r\n", e))).await; cleanup(&username).await; return; }
    };

    if let Err(e) = channel.request_pty("xterm-256color", None, Some((80, 24, 0, 0))) {
        let _ = ws.send(Message::Text(format!("\r\n❌ PTY 请求失败: {}\r\n", e))).await;
        cleanup(&username).await; return;
    }
    if let Err(e) = channel.shell() {
        let _ = ws.send(Message::Text(format!("\r\n❌ 启动 Shell 失败: {}\r\n", e))).await;
        cleanup(&username).await; return;
    }

    let _ = ws.send(Message::Text("\r\n\x1b[32m✅ 已连接\x1b[0m\r\n\r\n".into())).await;
    tracing::info!("[SSH] {} 已成功连接到 {}@{}:{}", username, params.username, params.host, params.port);

    // 2. 非阻塞模式
    sess.set_blocking(false);
    let _sess = std::sync::Arc::new(std::sync::Mutex::new(sess));
    let channel = std::sync::Arc::new(std::sync::Mutex::new(channel));

    // 3. SSH stdout → channel tx → WebSocket
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let reader_ch = channel.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            let data = {
                let mut ch = reader_ch.lock().unwrap();
                match ch.read(&mut buf) {
                    Ok(0) => None,                            // EOF
                    Ok(n) => Some(buf[..n].to_vec()),         // data
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(30));
                        if ch.eof() { None } else { continue; }
                    }
                    Err(_) => None,
                }
            };
            match data {
                Some(d) => { if tx.send(d).is_err() { break; } }
                None => break,
            }
        }
    });

    // 4. 终端大小调整消息解析
    let resize_ch = channel.clone();

    // 5. 主循环: WebSocket ↔ SSH 双向桥接
    loop {
        tokio::select! {
            // SSH stdout → WebSocket
            data = rx.recv() => {
                match data {
                    Some(bytes) => {
                        if ws.send(Message::Binary(bytes)).await.is_err() { break; }
                    }
                    None => break, // SSH reader closed
                }
            }
            // WebSocket → SSH stdin (or resize)
            msg = ws.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        // 检查是否为 resize 消息 {cols,rows}
                        if let Ok(resize) = serde_json::from_str::<serde_json::Value>(&text) {
                            if resize.get("cols").and_then(|v| v.as_u64()).is_some() {
                                let cols = resize["cols"].as_u64().unwrap_or(80) as u32;
                                let rows = resize["rows"].as_u64().unwrap_or(24) as u32;
                                let ch = resize_ch.clone();
                                let _ = tokio::task::spawn_blocking(move || {
                                    let mut ch = ch.lock().unwrap();
                                    let _ = ch.request_pty_size(cols, rows, None, None);
                                }).await;
                                continue;
                            }
                        }
                        // 普通文本 → SSH stdin
                        let ch = channel.clone();
                        let data = text.into_bytes();
                        let _ = tokio::task::spawn_blocking(move || {
                            let mut ch = ch.lock().unwrap();
                            let _ = ch.write_all(&data);
                        }).await;
                    }
                    Some(Ok(Message::Binary(data))) => {
                        let ch = channel.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            let mut ch = ch.lock().unwrap();
                            let _ = ch.write_all(&data);
                        }).await;
                    }
                    _ => break, // Close or error → disconnect
                }
            }
        }
    }

    cleanup(&username).await;
    tracing::info!("[SSH] {} 已断开 SSH", username);
}

async fn cleanup(username: &str) {
    ACTIVE_SESSIONS.lock().await.remove(username);
}
