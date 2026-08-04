// ====== Minecraft 服务器状态查询 ======
use crate::error::{AppError, AppResult};
use axum::{extract::Extension, response::Json};
use pinas_core::UserSession;
use serde::Serialize;

// ====== VarInt 编解码 ======

fn write_varint(buf: &mut Vec<u8>, value: i32) {
    let mut v = value as u32;
    loop {
        if v & !0x7F == 0 {
            buf.push(v as u8);
            break;
        }
        buf.push(((v & 0x7F) | 0x80) as u8);
        v >>= 7;
    }
}

fn write_string(buf: &mut Vec<u8>, s: &str) {
    write_varint(buf, s.len() as i32);
    buf.extend_from_slice(s.as_bytes());
}

fn encode_packet(packet_id: i32, payload: &[u8]) -> Vec<u8> {
    let mut inner = Vec::new();
    write_varint(&mut inner, packet_id);
    inner.extend_from_slice(payload);

    let mut packet = Vec::new();
    write_varint(&mut packet, inner.len() as i32);
    packet.extend_from_slice(&inner);
    packet
}

async fn read_varint_async(stream: &mut tokio::net::TcpStream) -> Result<i32, String> {
    let mut value: i32 = 0;
    let mut buf = [0u8; 1];
    for i in 0..5 {
        tokio::io::AsyncReadExt::read_exact(stream, &mut buf)
            .await
            .map_err(|e| format!("读取 VarInt 失败: {}", e))?;
        let byte = buf[0];
        value |= ((byte & 0x7F) as i32)
            .checked_shl((i * 7) as u32)
            .unwrap_or(0);
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err("VarInt 过长（超过 5 字节）".into())
}

// ====== MOTD 提取 ======

fn extract_motd(desc: &serde_json::Value) -> String {
    match desc {
        serde_json::Value::String(s) => strip_mc_colors(s),
        serde_json::Value::Object(obj) => {
            let mut parts = Vec::new();
            if let Some(text) = obj.get("text").and_then(|v| v.as_str())
                && !text.is_empty()
            {
                parts.push(strip_mc_colors(text));
            }
            if let Some(extra) = obj.get("extra").and_then(|v| v.as_array()) {
                for item in extra {
                    let t = extract_motd(item);
                    if !t.is_empty() {
                        parts.push(t);
                    }
                }
            }
            if parts.is_empty() {
                // Fallback: serialize the whole object
                serde_json::to_string(obj).unwrap_or_default()
            } else {
                parts.join("")
            }
        }
        _ => String::new(),
    }
}

fn strip_mc_colors(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{00a7}' {
            chars.next(); // 跳过颜色代码字符
        } else {
            out.push(c);
        }
    }
    out
}

// ====== 响应结构体 ======

#[derive(Serialize)]
pub struct McServerStatus {
    pub online: bool,
    pub version: Option<String>,
    pub protocol: Option<i32>,
    pub motd: Option<String>,
    pub players_online: Option<i32>,
    pub players_max: Option<i32>,
    pub player_names: Vec<String>,
    pub error: Option<String>,
}

impl McServerStatus {
    fn offline(error: String) -> Self {
        Self {
            online: false,
            version: None,
            protocol: None,
            motd: None,
            players_online: None,
            players_max: None,
            player_names: vec![],
            error: Some(error),
        }
    }
}

// ====== 核心查询函数 ======

async fn query_mc_server(host: &str, port: u16) -> McServerStatus {
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpStream;

    let addr = format!("{}:{}", host, port);

    // 连接（3 秒超时）
    let mut stream =
        match tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(&addr)).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return McServerStatus::offline(format!("连接失败: {}", e)),
            Err(_) => return McServerStatus::offline("连接超时".into()),
        };

    // 构建 Handshake 包
    let mut handshake_payload = Vec::new();
    write_varint(&mut handshake_payload, -1); // protocol version（自动协商）
    write_string(&mut handshake_payload, host);
    handshake_payload.extend_from_slice(&port.to_be_bytes());
    write_varint(&mut handshake_payload, 1); // next state: status

    let handshake = encode_packet(0x00, &handshake_payload);

    // Status Request 包（包 ID 0x00，无载荷）
    let status_request = vec![0x01u8, 0x00u8];

    // 发送数据（2 秒超时）
    match tokio::time::timeout(Duration::from_secs(2), async {
        stream.write_all(&handshake).await?;
        stream.write_all(&status_request).await?;
        stream.flush().await?;
        Ok::<_, std::io::Error>(())
    })
    .await
    {
        Ok(Ok(())) => {} // 发送成功
        Ok(Err(io_err)) => return McServerStatus::offline(format!("发送数据失败: {}", io_err)),
        Err(_) => return McServerStatus::offline("发送数据超时".into()),
    }

    // 读取响应（2 秒超时）
    let response_bytes = match tokio::time::timeout(Duration::from_secs(2), async {
        // 读取包长度（VarInt）
        let _packet_len = read_varint_async(&mut stream)
            .await
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        // 读取包 ID
        let packet_id = read_varint_async(&mut stream)
            .await
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        if packet_id != 0x00 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("意外的包 ID: 0x{:02X}", packet_id),
            ));
        }

        // 读取 JSON 字符串长度
        let json_len = read_varint_async(&mut stream)
            .await
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        if !(0..=65536).contains(&json_len) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("JSON 长度异常: {}", json_len),
            ));
        }

        // 读取 JSON 数据
        let mut json_bytes = vec![0u8; json_len as usize];
        tokio::io::AsyncReadExt::read_exact(&mut stream, &mut json_bytes).await?;

        Ok::<_, std::io::Error>(json_bytes)
    })
    .await
    {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => return McServerStatus::offline(format!("读取响应失败: {}", e)),
        Err(_) => return McServerStatus::offline("读取响应超时".into()),
    };

    let response = match String::from_utf8(response_bytes) {
        Ok(s) => s,
        Err(e) => return McServerStatus::offline(format!("UTF-8 解码失败: {}", e)),
    };
    // 解析 JSON
    let parsed: serde_json::Value = match serde_json::from_str(&response) {
        Ok(v) => v,
        Err(e) => return McServerStatus::offline(format!("JSON 解析失败: {}", e)),
    };

    // 提取各字段
    let version = parsed
        .get("version")
        .and_then(|v| v.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let protocol = parsed
        .get("version")
        .and_then(|v| v.get("protocol"))
        .and_then(|v| v.as_i64())
        .map(|n| n as i32);

    let players_online = parsed
        .get("players")
        .and_then(|v| v.get("online"))
        .and_then(|v| v.as_i64())
        .map(|n| n as i32);

    let players_max = parsed
        .get("players")
        .and_then(|v| v.get("max"))
        .and_then(|v| v.as_i64())
        .map(|n| n as i32);

    let player_names: Vec<String> = parsed
        .get("players")
        .and_then(|v| v.get("sample"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    p.get("name")
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string())
                })
                .collect()
        })
        .unwrap_or_default();

    let motd = parsed
        .get("description")
        .map(extract_motd)
        .filter(|s| !s.is_empty());

    McServerStatus {
        online: true,
        version,
        protocol,
        motd,
        players_online,
        players_max,
        player_names,
        error: None,
    }
}

// ====== Axum Handler ======

pub async fn get_minecraft_status(
    Extension(session): Extension<UserSession>,
) -> AppResult<Json<serde_json::Value>> {
    // 仅管理员可查看
    if session.role != crate::constants::ROLE_ADMIN {
        return Err(AppError::forbidden("管理员权限不足"));
    }

    let host = std::env::var("MINECRAFT_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: u16 = std::env::var("MINECRAFT_PORT")
        .unwrap_or_else(|_| "25565".into())
        .parse()
        .unwrap_or(25565);
    let status = query_mc_server(&host, port).await;
    let json = serde_json::to_value(&status).unwrap_or(serde_json::json!({
        "online": false,
        "error": "序列化失败"
    }));

    Ok(Json(json))
}

// ====== HTMX Fragment ======
use crate::templates::AppTemplate;
use askama::Template;

#[derive(Template)]
#[template(path = "components/minecraft_status.html")]
pub struct MinecraftStatusFragment {
    online: bool,
    motd: String,
    version: String,
    players: String,
    player_names: String,
    error: String,
}

#[tracing::instrument(skip_all)]
pub async fn minecraft_status_fragment(
    axum::extract::Extension(session): axum::extract::Extension<pinas_core::UserSession>,
) -> Result<AppTemplate<MinecraftStatusFragment>, axum::http::StatusCode> {
    // 与 get_minecraft_status 对齐：仅管理员可见（MC 状态含内网拓扑信息）
    if session.role != crate::constants::ROLE_ADMIN {
        return Err(axum::http::StatusCode::FORBIDDEN);
    }
    // Default config — same as get_minecraft_status
    let host = std::env::var("MINECRAFT_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: u16 = std::env::var("MINECRAFT_PORT")
        .unwrap_or_else(|_| "25565".into())
        .parse()
        .unwrap_or(25565);

    match query_mc_server(&host, port).await {
        McServerStatus {
            online: true,
            motd,
            version,
            players_online,
            players_max,
            player_names,
            ..
        } => {
            let players = format!(
                "{} / {}",
                players_online.unwrap_or(0),
                players_max.unwrap_or(0)
            );
            let names = player_names.join(", ");
            Ok(AppTemplate(MinecraftStatusFragment {
                online: true,
                motd: motd.unwrap_or_else(|| "---".into()),
                version: version.unwrap_or_else(|| "---".into()),
                players,
                player_names: if names.is_empty() {
                    String::new()
                } else {
                    names
                },
                error: String::new(),
            }))
        }
        status => {
            let err = status.error.unwrap_or_else(|| "无法连接".into());
            Ok(AppTemplate(MinecraftStatusFragment {
                online: false,
                motd: "---".into(),
                version: "---".into(),
                players: "---".into(),
                player_names: String::new(),
                error: err,
            }))
        }
    }
}
