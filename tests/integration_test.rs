// ====== Antifield Cloud 集成测试 ======
// 使用内存 SQLite + tower::ServiceExt 进行端到端 HTTP 测试

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use pi_nas::config::Config;
use pi_nas::db;
use pi_nas::router;
use sqlx::SqlitePool;
use tower::util::ServiceExt;

/// 进程级共享：首次调用把 CWD 切换到独立临时目录（uploads/ 不再污染项目目录）。
/// 所有 test_app* 辅助函数共用同一个 Once——各自独立会导致测试中途切换 CWD
static CWD_INIT: std::sync::Once = std::sync::Once::new();

fn ensure_test_cwd() {
    CWD_INIT.call_once(|| {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        // tempdir 存活到进程结束（测试共用同一 CWD，用户目录按 username 天然隔离）
        std::mem::forget(dir);
    });
}

async fn test_pool() -> SqlitePool {
    // foreign_keys 经连接选项设置，确保池中每条连接都启用 FK 约束
    sqlx::sqlite::SqlitePoolOptions::new()
        .connect_with(
            "sqlite::memory:"
                .parse::<sqlx::sqlite::SqliteConnectOptions>()
                .unwrap()
                .foreign_keys(true),
        )
        .await
        .unwrap()
}

/// 创建测试应用（内存 SQLite，仅建表不含默认用户）
async fn test_app() -> (SqlitePool, axum::Router) {
    ensure_test_cwd();
    let pool = test_pool().await;
    db::init_test_db(&pool).await.unwrap();
    let config = Config::default();
    let app = router::build_router(config, pool.clone());
    (pool, app)
}

/// 创建带自定义配置的测试应用（F18 等需改配额/开关的测试用）
async fn test_app_with_config(config: Config) -> (SqlitePool, axum::Router) {
    ensure_test_cwd();
    let pool = test_pool().await;
    db::init_test_db(&pool).await.unwrap();
    let app = router::build_router(config, pool.clone());
    (pool, app)
}

/// 辅助函数：通过 JSON body 发送 POST 请求
fn post_json(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// 辅助函数：发送带 Bearer token 的 GET 请求
fn get_with_token(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("authorization", format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap()
}

/// 辅助函数：发送带 Bearer token 的 POST 请求
fn post_json_with_token(uri: &str, body: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", token))
        .body(Body::from(body.to_string()))
        .unwrap()
}

// ====== 1. 认证流程测试 ======

#[tokio::test]
async fn test_auth_register_login_logout() {
    let (_pool, app) = test_app().await;

    // 注册
    let response = app
        .clone()
        .oneshot(post_json(
            "/api/register",
            r#"{"username":"alice","password":"secret123"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 重复注册应返回 CONFLICT
    let response = app
        .clone()
        .oneshot(post_json(
            "/api/register",
            r#"{"username":"alice","password":"secret123"}"#,
        ))
        .await
        .unwrap();
    assert!(
        response.status() == StatusCode::CONFLICT || response.status() == StatusCode::BAD_REQUEST
    );

    // 正确登录
    let response = app
        .clone()
        .oneshot(post_json(
            "/api/login",
            r#"{"username":"alice","password":"secret123"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    let login_data: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let token = login_data["token"].as_str().unwrap().to_string();

    // 使用 token 访问受保护路由
    let response = app
        .clone()
        .oneshot(get_with_token("/api/files/list", &token))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 错误密码登录应返回 401
    let response = app
        .clone()
        .oneshot(post_json(
            "/api/login",
            r#"{"username":"alice","password":"wrongpass"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // 登出
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/logout")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 登出后 token 应失效
    let response = app
        .oneshot(get_with_token("/api/files/list", &token))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// ====== 1b. logout 必须清理 Cookie 登录的会话（浏览器场景） ======

#[tokio::test]
async fn test_logout_clears_cookie_session() {
    let (_pool, app) = test_app().await;

    // 注册并登录，捕获 Set-Cookie
    let _ = app
        .clone()
        .oneshot(post_json(
            "/api/register",
            r#"{"username":"cookie_user","password":"secret123"}"#,
        ))
        .await
        .unwrap();
    let response = app
        .clone()
        .oneshot(post_json(
            "/api/login",
            r#"{"username":"cookie_user","password":"secret123"}"#,
        ))
        .await
        .unwrap();
    let cookie = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        cookie.contains("auth_token="),
        "登录响应应设置 auth_token Cookie"
    );
    let auth_cookie = cookie.split(';').next().unwrap().to_string(); // "auth_token=xxx"

    // 带 Cookie 访问受保护路由应成功
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/files/list")
                .header("cookie", &auth_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 仅带 Cookie（无 Authorization 头）登出
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/logout")
                .header("cookie", &auth_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cleared = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(cleared.contains("Max-Age=0"), "登出应清除 Cookie");

    // 同一 Cookie 再次访问应 401（服务端 session 行已删除）
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/files/list")
                .header("cookie", &auth_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// ====== 1c. change_password 在 HTTPS 反代场景必须设置 Secure Cookie ======

#[tokio::test]
async fn test_change_password_sets_secure_flag() {
    let (_pool, app) = test_app().await;

    let _ = app
        .clone()
        .oneshot(post_json(
            "/api/register",
            r#"{"username":"secure_user","password":"secret123"}"#,
        ))
        .await
        .unwrap();
    let response = app
        .clone()
        .oneshot(post_json(
            "/api/login",
            r#"{"username":"secure_user","password":"secret123"}"#,
        ))
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    let login: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let token = login["token"].as_str().unwrap().to_string();

    // 带 X-Forwarded-Proto: https 修改密码 → 新会话 Cookie 必须带 Secure
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/user/password")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .header("x-forwarded-proto", "https")
                .body(Body::from(
                    r#"{"current_password":"secret123","new_password":"newpass456"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        cookie.contains("; Secure"),
        "HTTPS 场景下改密后 Cookie 应带 Secure: {}",
        cookie
    );

    // 无 X-Forwarded-Proto（直连 http）→ 不带 Secure，与 login 行为一致
    let login2 = app
        .clone()
        .oneshot(post_json(
            "/api/login",
            r#"{"username":"secure_user","password":"newpass456"}"#,
        ))
        .await
        .unwrap();
    let body2 = axum::body::to_bytes(login2.into_body(), 1024)
        .await
        .unwrap();
    let login2: serde_json::Value = serde_json::from_slice(&body2).unwrap();
    let token2 = login2["token"].as_str().unwrap().to_string();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/user/password")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token2))
                .body(Body::from(
                    r#"{"current_password":"newpass456","new_password":"thirdpass789"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie2 = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap();
    // F15 (v1.7.0)：Cookie 默认强制 Secure（部署于 CF 隧道后）——纯 HTTP 局域网需
    // 显式 PINAS_COOKIE_SECURE=false 才会关闭
    assert!(
        cookie2.contains("; Secure"),
        "默认配置下 Cookie 应始终带 Secure: {}",
        cookie2
    );
}

// ====== 2. 文件 CRUD 测试 ======

#[tokio::test]
async fn test_file_create_list_delete_restore() {
    let (_pool, app) = test_app().await;

    // 注册并登录
    let _ = app
        .clone()
        .oneshot(post_json(
            "/api/register",
            r#"{"username":"bob","password":"pass456"}"#,
        ))
        .await
        .unwrap();
    let response = app
        .clone()
        .oneshot(post_json(
            "/api/login",
            r#"{"username":"bob","password":"pass456"}"#,
        ))
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    let token: String = {
        let data: serde_json::Value = serde_json::from_slice(&body).unwrap();
        data["token"].as_str().unwrap().to_string()
    };

    // 创建文件夹
    let response = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/create_folder",
            r#"{"name":"docs","current_path":null}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 列出文件（验证文件夹存在）
    let response = app
        .clone()
        .oneshot(get_with_token("/api/files/list?path=", &token))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let list: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let items = list["items"].as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|f| f["name"] == "docs" && f["is_dir"] == 1)
    );

    // 重命名文件夹
    let response = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/rename",
            r#"{"name":"docs","new_name":"documents","current_path":null}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 验证重命名
    let response = app
        .clone()
        .oneshot(get_with_token("/api/files/list?path=", &token))
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let list: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let items = list["items"].as_array().unwrap();
    assert!(items.iter().any(|f| f["name"] == "documents"));
    assert!(!items.iter().any(|f| f["name"] == "docs"));

    // 删除（移至回收站）
    let response = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/delete",
            r#"{"name":"documents","current_path":null}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 回收站中应包含已删除项
    let response = app
        .clone()
        .oneshot(get_with_token("/api/trash/list", &token))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let trash: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert!(!trash.is_empty());
    let trash_id = trash[0]["id"].as_i64().unwrap();

    // 从回收站还原
    let response = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/trash/restore",
            &format!(r#"{{"id":{}}}"#, trash_id),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 验证文件已还原
    let response = app
        .oneshot(get_with_token("/api/files/list?path=", &token))
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let list: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let items = list["items"].as_array().unwrap();
    assert!(items.iter().any(|f| f["name"] == "documents"));
}

// ====== 3. 分享流程测试 ======

#[tokio::test]
async fn test_share_create_access_delete() {
    let (_pool, app) = test_app().await;

    // 注册并登录
    let _ = app
        .clone()
        .oneshot(post_json(
            "/api/register",
            r#"{"username":"carol","password":"share123"}"#,
        ))
        .await
        .unwrap();
    let response = app
        .clone()
        .oneshot(post_json(
            "/api/login",
            r#"{"username":"carol","password":"share123"}"#,
        ))
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    let token: String = {
        let data: serde_json::Value = serde_json::from_slice(&body).unwrap();
        data["token"].as_str().unwrap().to_string()
    };

    // 创建分享（无密码，1小时过期）
    let response = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/share/create",
            r#"{"file_path":"/test.txt","is_dir":false,"expire_hours":1}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    let share_data: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let share_code = share_data["code"].as_str().unwrap().to_string();
    // 验证为完整 UUID（36 字符，含连字符）
    assert_eq!(share_code.len(), 36);
    assert!(share_code.contains('-'));

    // 列出分享
    let response = app
        .clone()
        .oneshot(get_with_token("/api/share/list", &token))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let shares: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert!(shares.iter().any(|s| s["code"] == share_code));

    // 真实创建被分享的文件（access 端点现在要求磁盘文件存在）
    tokio::fs::create_dir_all("uploads/carol").await.unwrap();
    tokio::fs::write("uploads/carol/test.txt", b"share-content")
        .await
        .unwrap();

    // 匿名访问分享 → 应返回文件流（attachment 下载）
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/share/access/{}", share_code))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get("content-disposition")
            .map(|v| v.to_str().unwrap_or("").contains("attachment"))
            .unwrap_or(false),
        "下载响应必须带 Content-Disposition: attachment"
    );

    // 清理
    let _ = tokio::fs::remove_file("uploads/carol/test.txt").await;

    // 删除分享
    let response = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/share/delete",
            &format!(r#"{{"code":"{}"}}"#, share_code),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 访问已删除的分享应返回 GONE 或 NOT_FOUND
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/share/access/{}", share_code))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(response.status() == StatusCode::GONE || response.status() == StatusCode::NOT_FOUND);
}

// ====== 4. 健康检查测试 ======

#[tokio::test]
async fn test_health_check() {
    let (_pool, app) = test_app().await;

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    let health: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(health["status"], "healthy");
    assert_eq!(health["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(health["database"], "connected");
}

// ====== 5. 上传分片→合并流程测试（真实 multipart 端到端） ======

const CHUNK_SIZE: usize = 64 * 1024;

/// 手工构造 multipart/form-data 请求体（字段名为 file）
fn multipart_chunk_body(filename: &str, data: &[u8], boundary: &str) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n",
            filename
        )
        .as_bytes(),
    );
    body.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{}--\r\n", boundary).as_bytes());
    body
}

/// 上传一个分片并断言成功
async fn upload_one_chunk(
    app: &axum::Router,
    token: &str,
    identifier: &str,
    index: i32,
    total: i32,
    data: &[u8],
) {
    let boundary = "test-boundary-7MA4YWxkTrZu0gW";
    let body = multipart_chunk_body("chunked.bin", data, boundary);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/files/upload_chunk?identifier={}&chunk_index={}&total_chunks={}",
                    identifier, index, total
                ))
                .header("authorization", format!("Bearer {}", token))
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={}", boundary),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "分片 {} 上传失败", index);
}

#[tokio::test]
async fn test_upload_chunk_and_merge() {
    let (_pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;

    let identifier = "test-upload-e2e";

    // 1. 检查 — 应无已上传分片
    let resp = app
        .clone()
        .oneshot(get_with_token(
            &format!("/api/files/check?identifier={}", identifier),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let check: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(!check["exists"].as_bool().unwrap());

    // 2. 上传 3 个 64KB 分片（ASCII 文本内容，可区分且通过 MIME 完整检测）
    let chunks: Vec<Vec<u8>> = (0..3)
        .map(|i| {
            let pattern = format!("CHUNK-{}-", i).into_bytes();
            let mut v = vec![0u8; CHUNK_SIZE];
            for (j, b) in v.iter_mut().enumerate() {
                *b = pattern[j % pattern.len()];
            }
            v
        })
        .collect();
    for (i, data) in chunks.iter().enumerate() {
        upload_one_chunk(&app, &token, identifier, i as i32, 3, data).await;
    }

    // 3. 断点续传检查 — 已上传分片应完整列出
    let resp = app
        .clone()
        .oneshot(get_with_token(
            &format!("/api/files/check?identifier={}", identifier),
            &token,
        ))
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 2048).await.unwrap();
    let check: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(!check["exists"].as_bool().unwrap());
    let uploaded: Vec<i32> = check["uploaded_chunks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap() as i32)
        .collect();
    assert_eq!(uploaded, vec![0, 1, 2], "断点续传应报告全部已上传分片");

    // 4. 合并
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/merge",
            &format!(
                r#"{{"identifier":"{}","file_name":"chunked.bin","parent_path":""}}"#,
                identifier
            ),
            &token,
        ))
        .await
        .unwrap();
    let status = resp.status();
    if status != StatusCode::OK {
        let b = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        panic!("合并失败: {} body={}", status, String::from_utf8_lossy(&b));
    }

    // 5. 列表包含该文件
    let resp = app
        .clone()
        .oneshot(get_with_token("/api/files/list?path=", &token))
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let list: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let items = list["items"].as_array().unwrap();
    assert!(
        items.iter().any(|f| f["name"] == "chunked.bin"),
        "列表应包含合并后的文件"
    );

    // 6. 磁盘文件字节完整性 = 3 × CHUNK_SIZE
    let disk_path = format!("uploads/{}/chunked.bin", username);
    let meta = tokio::fs::metadata(&disk_path)
        .await
        .expect("磁盘文件应存在");
    assert_eq!(
        meta.len() as usize,
        3 * CHUNK_SIZE,
        "合并文件字节数应等于分片总和"
    );
    let content = tokio::fs::read(&disk_path).await.unwrap();
    for (i, data) in chunks.iter().enumerate() {
        assert_eq!(
            &content[i * CHUNK_SIZE..(i + 1) * CHUNK_SIZE],
            data.as_slice(),
            "分片 {} 内容应原样保留",
            i
        );
    }

    // 7. 清理测试产物（避免污染后续测试）
    let _ = tokio::fs::remove_file(&disk_path).await;
}

// ====== 5b. 配额强制测试 — 超出配额合并应 403 ======

#[tokio::test]
async fn test_upload_exceeds_quota_forbidden() {
    let (pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;

    // 配额归零
    sqlx::query("UPDATE users SET quota_mb = 0, used_mb = 0 WHERE username = ?")
        .bind(&username)
        .execute(&pool)
        .await
        .unwrap();

    let identifier = "test-quota-limit";
    let data = vec![0xAB; CHUNK_SIZE];
    upload_one_chunk(&app, &token, identifier, 0, 1, &data).await;

    // 合并应被配额拦截
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/merge",
            &format!(
                r#"{{"identifier":"{}","file_name":"quota.bin","parent_path":""}}"#,
                identifier
            ),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "超配额合并应返回 403");
    let body = axum::body::to_bytes(resp.into_body(), 2048).await.unwrap();
    let text = String::from_utf8_lossy(&body).to_string();
    assert!(
        text.contains("存储空间不足"),
        "应返回通用配额提示: {}",
        text
    );
}

// ====== 6. 分享密码验证测试（完整流程：表单→错误密码→正确密码→删除） ======

#[tokio::test]
async fn test_share_with_password() {
    let (_pool, app) = test_app().await;
    let token = register_and_login(&app).await;

    // 创建真实目录，确保分享页能识别 is_dir
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/create_folder",
            r#"{"name":"shared_doc","current_path":null}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 创建带密码的目录分享
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/share/create",
            r#"{"file_path":"shared_doc","is_dir":true,"password":"secret456"}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let share: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let code = share["code"].as_str().unwrap().to_string();

    // 1. 无密码访问 → 显示提取码表单
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/s/{}", code))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let html = String::from_utf8_lossy(&body).to_string();
    assert!(html.contains("验证提取码"), "应渲染密码表单: {}", html);

    // 2. 错误密码 → 仍回表单
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/s/{}?password=wrongpass", code))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let html = String::from_utf8_lossy(&body).to_string();
    assert!(html.contains("验证提取码"), "错误密码应仍显示表单");

    // 3. 正确密码 → 显示目录浏览页
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/s/{}?password=secret456", code))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let html = String::from_utf8_lossy(&body).to_string();
    assert!(
        html.contains("浏览目录"),
        "正确密码应显示目录浏览入口: {}",
        html
    );

    // 4. 删除分享 → 404
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/share/delete",
            &format!(r#"{{"code":"{}"}}"#, code),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/s/{}", code))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "删除后应 404");
}

// ====== 7. 回收站操作测试 ======

#[tokio::test]
async fn test_trash_operations() {
    let (_pool, app) = test_app().await;
    let token = register_and_login(&app).await;

    // 检查回收站列表（新用户应为空）
    let resp = app
        .clone()
        .oneshot(get_with_token("/api/trash/list", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ====== 8. 链接收藏 CRUD 测试 ======

#[tokio::test]
async fn test_links_crud() {
    let (_pool, app) = test_app().await;

    // 使用 alice 账号
    let _ = app
        .clone()
        .oneshot(post_json(
            "/api/register",
            r#"{"username":"alice_links","password":"secret123"}"#,
        ))
        .await;
    let login_resp = app
        .clone()
        .oneshot(post_json(
            "/api/login",
            r#"{"username":"alice_links","password":"secret123"}"#,
        ))
        .await
        .unwrap();
    let body = axum::body::to_bytes(login_resp.into_body(), 1024)
        .await
        .unwrap();
    let login: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let token = login["token"].as_str().unwrap();

    // 创建链接
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/links",
            r#"{"title":"Test Link","url":"https://example.com"}"#,
            token,
        ))
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "创建链接失败: {}",
        resp.status()
    );

    // 列出链接
    let resp = app
        .clone()
        .oneshot(get_with_token("/api/links", token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ====== 9. 待办 CRUD 测试 ======

#[tokio::test]
async fn test_todos_crud() {
    let (_pool, app) = test_app().await;

    // 注册新用户
    let _ = app
        .clone()
        .oneshot(post_json(
            "/api/register",
            r#"{"username":"alice_todos","password":"secret123"}"#,
        ))
        .await;
    let login_resp = app
        .clone()
        .oneshot(post_json(
            "/api/login",
            r#"{"username":"alice_todos","password":"secret123"}"#,
        ))
        .await
        .unwrap();
    let body = axum::body::to_bytes(login_resp.into_body(), 1024)
        .await
        .unwrap();
    let login: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let token = login["token"].as_str().unwrap();

    // 创建待办
    let resp = app.clone().oneshot(post_json_with_token(
        "/api/todos",
        r#"{"title":"Test Todo","description":"test","priority":"medium","category":"todo","status":"pending"}"#,
        token,
    )).await.unwrap();
    assert!(
        resp.status().is_success(),
        "创建待办失败: {}",
        resp.status()
    );

    // 列出待办
    let resp = app
        .clone()
        .oneshot(get_with_token("/api/todos", token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ====== 辅助函数 ======

async fn register_and_login(app: &axum::Router) -> String {
    register_and_login_with_username(app).await.0
}

/// 注册并登录，返回 (token, username)
async fn register_and_login_with_username(app: &axum::Router) -> (String, String) {
    // 原子计数器保证进程内唯一：并行测试下毫秒时间戳可能碰撞（共享限速器等全局状态互相干扰）
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let username = format!("testuser_{}_{}", std::process::id(), n);

    // 注册
    let resp = app
        .clone()
        .oneshot(post_json(
            "/api/register",
            &format!(r#"{{"username":"{}","password":"testpass123"}}"#, username),
        ))
        .await
        .unwrap();
    assert!(resp.status().is_success(), "注册失败: {}", resp.status());

    // 登录
    let resp = app
        .clone()
        .oneshot(post_json(
            "/api/login",
            &format!(r#"{{"username":"{}","password":"testpass123"}}"#, username),
        ))
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let login: serde_json::Value = serde_json::from_slice(&body).unwrap();
    (login["token"].as_str().unwrap().to_string(), username)
}

// ====== 9. 安全回归测试 (v1.5.1) ======

/// merge 的 file_name 必须拒绝路径穿越（C1 任意文件写入漏洞回归）
#[tokio::test]
async fn test_merge_rejects_path_traversal_filename() {
    let (_pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;

    // 先上传一个有效分片
    upload_one_chunk(&app, &token, "sec-trav-001", 0, 1, b"hello world").await;

    // 尝试用穿越文件名合并 → 必须 400，且沙箱外不得出现文件
    for evil_name in [
        "../../escape_test.txt",
        "../escape_test.txt",
        "sub/../../escape_test.txt",
        "..",
        "/etc/escape_test.txt",
    ] {
        let resp = app
            .clone()
            .oneshot(post_json_with_token(
                "/api/files/merge",
                &format!(
                    r#"{{"identifier":"sec-trav-001","file_name":"{}","parent_path":"/"}}"#,
                    evil_name
                ),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "穿越文件名 '{}' 应被拒绝",
            evil_name
        );
    }
    // 沙箱外不得出现文件
    assert!(
        !std::path::Path::new("escape_test.txt").exists(),
        "沙箱外不应有写入文件"
    );
    assert!(
        !std::path::Path::new(&format!("uploads/{}/escape_test.txt", username)).exists(),
        "沙箱内也不应有 escape_test.txt"
    );
}

/// delete name=".." 不得移走整个 uploads（P0.2 沙箱 Result 回归）
#[tokio::test]
async fn test_delete_parent_dir_rejected() {
    let (_pool, app) = test_app().await;
    let (token, _username) = register_and_login_with_username(&app).await;

    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/delete",
            r#"{"name":"..","current_path":"/"}"#,
            &token,
        ))
        .await
        .unwrap();
    assert!(
        resp.status().is_client_error(),
        "name='..' 删除应被拒绝，实际: {}",
        resp.status()
    );
    // uploads 目录必须完好
    assert!(
        std::path::Path::new("uploads").is_dir(),
        "uploads 目录必须完整存在"
    );
}

/// 创建文件夹名称含引号/尖括号 → 400（XSS 上游防线回归）
#[tokio::test]
async fn test_create_folder_rejects_unsafe_chars() {
    let (_pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;

    for evil in [
        "evil'folder",
        "evil\"folder",
        "evil<folder",
        "evil>folder",
        "../evil",
    ] {
        let resp = app
            .clone()
            .oneshot(post_json_with_token(
                "/api/files/create_folder",
                &format!(r#"{{"name":"{}","current_path":"/"}}"#, evil),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "名称 '{}' 应被拒绝",
            evil
        );
        assert!(
            !std::path::Path::new(&format!("uploads/{}/{}", username, evil)).exists(),
            "非法名称不应创建目录: {}",
            evil
        );
    }
}

/// 重命名为含引号名称 → 400（XSS 上游防线回归）
#[tokio::test]
async fn test_rename_rejects_unsafe_name() {
    let (_pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;

    // 先创建合法目录
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/create_folder",
            r#"{"name":"okdir","current_path":"/"}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 重命名为含引号名称 → 400
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/rename",
            r#"{"name":"okdir","new_name":"bad'name","current_path":"/"}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 原目录仍在
    assert!(
        std::path::Path::new(&format!("uploads/{}/okdir", username)).is_dir(),
        "原目录应保留"
    );
}

/// 分享文件下载响应必须带 Content-Disposition: attachment（P0.5 同源 XSS 回归）
#[tokio::test]
async fn test_share_download_has_attachment_disposition() {
    let (_pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;

    // 上传一个文件
    upload_one_chunk(&app, &token, "sec-share-001", 0, 1, b"malicious-content").await;
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/merge",
            r#"{"identifier":"sec-share-001","file_name":"evil.html","parent_path":""}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 创建分享
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/share/create",
            r#"{"file_path":"evil.html","is_dir":false}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let share: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let code = share["code"].as_str().unwrap().to_string();

    // 访问分享下载端点 → 必须 attachment 且 Content-Type 为 octet-stream（html 强制下载）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/share/access/{}", code))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let headers = resp.headers();
    assert!(
        headers
            .get("content-disposition")
            .map(|v| v.to_str().unwrap_or("").contains("attachment"))
            .unwrap_or(false),
        "分享文件必须 Content-Disposition: attachment"
    );
    assert_eq!(
        headers.get("content-type").unwrap(),
        "application/octet-stream",
        "html 分享必须强制 octet-stream"
    );

    // 清理
    let _ = tokio::fs::remove_file(format!("uploads/{}/evil.html", username)).await;
}

// ====== 10. P2 补充测试：目录子树 rename/move + 媒体 Range ======

/// rename/move 后子目录的 DB parent_path 必须随子树迁移（update_child_parent_paths 回归）
#[tokio::test]
async fn test_rename_move_subtree_paths() {
    let (pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;

    // 创建目录树 a/b/c
    for path in ["a", "a/b", "a/b/c"] {
        let resp = app
            .clone()
            .oneshot(post_json_with_token(
                "/api/files/create_folder",
                &format!(
                    r#"{{"name":"{}","current_path":"{}"}}"#,
                    path.rsplit('/').next().unwrap(),
                    path.trim_end_matches(path.rsplit('/').next().unwrap())
                        .trim_end_matches('/')
                ),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "创建 {} 失败", path);
    }

    // 重命名 a → a2，子路径应迁移
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/rename",
            r#"{"name":"a","new_name":"a2","current_path":"/"}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "重命名失败");

    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT name, parent_path FROM files WHERE username = ?")
            .bind(&username)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(
        rows.iter().any(|(n, p)| n == "c" && p == "a2/b"),
        "rename 后 c 的 parent_path 应为 a2/b，实际: {:?}",
        rows
    );
    // 磁盘结构同样迁移
    assert!(std::path::Path::new(&format!("uploads/{}/a2/b/c", username)).is_dir());

    // 移动 a2 → x，子树应再次迁移
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/create_folder",
            r#"{"name":"x","current_path":"/"}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/move",
            r#"{"name":"a2","target_dir":"x","current_path":"/"}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "移动失败");

    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT name, parent_path FROM files WHERE username = ?")
            .bind(&username)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(
        rows.iter().any(|(n, p)| n == "c" && p == "x/a2/b"),
        "move 后 c 的 parent_path 应为 x/a2/b，实际: {:?}",
        rows
    );
    assert!(std::path::Path::new(&format!("uploads/{}/x/a2/b/c", username)).is_dir());
}

/// 回归测试：重命名/移动目标已存在时返回 409 且目标文件内容不被覆盖销毁
#[tokio::test]
async fn test_rename_move_conflict_preserves_target() {
    let (_pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;

    // 上传 a.bin（内容 A）与 b.bin（内容 B）
    let content_a: Vec<u8> = vec![b'A'; 1024];
    let content_b: Vec<u8> = vec![b'B'; 1024];
    upload_one_chunk(&app, &token, "conf-a", 0, 1, &content_a).await;
    upload_one_chunk(&app, &token, "conf-b", 0, 1, &content_b).await;
    for (ident, name) in [("conf-a", "a.bin"), ("conf-b", "b.bin")] {
        let resp = app
            .clone()
            .oneshot(post_json_with_token(
                "/api/files/merge",
                &format!(
                    r#"{{"identifier":"{}","file_name":"{}","parent_path":""}}"#,
                    ident, name
                ),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "上传 {} 失败", name);
    }

    // a.bin → b.bin 必须 409
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/rename",
            r#"{"name":"a.bin","new_name":"b.bin","current_path":"/"}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "重命名为已存在目标应 409"
    );

    // b.bin 的磁盘内容必须仍是 B（修复前已被 a.bin 覆盖销毁）
    let b_path = format!("uploads/{}/b.bin", username);
    let disk_b = tokio::fs::read(&b_path).await.unwrap();
    assert_eq!(disk_b, content_b, "b.bin 内容不得被覆盖");
    let a_path = format!("uploads/{}/a.bin", username);
    assert!(
        tokio::fs::try_exists(&a_path).await.unwrap(),
        "a.bin 应保持原名存在"
    );

    // 移动冲突：dir2 下已有 b2.bin，把根目录 b2.bin 移入 dir2 必须 409
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/create_folder",
            r#"{"name":"dir2","current_path":"/"}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    upload_one_chunk(&app, &token, "conf-b2", 0, 1, &content_b).await;
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/merge",
            r#"{"identifier":"conf-b2","file_name":"b2.bin","parent_path":"dir2"}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    upload_one_chunk(&app, &token, "conf-b2-root", 0, 1, &content_a).await;
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/merge",
            r#"{"identifier":"conf-b2-root","file_name":"b2.bin","parent_path":""}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/move",
            r#"{"name":"b2.bin","target_dir":"dir2","current_path":"/"}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "移入含同名文件的目录应 409"
    );
    let disk = tokio::fs::read(format!("uploads/{}/dir2/b2.bin", username))
        .await
        .unwrap();
    assert_eq!(disk, content_b, "dir2/b2.bin 内容不得被覆盖");
}

/// 回归测试：重传同名文件必须 409，且不销毁已存在的旧文件
#[tokio::test]
async fn test_merge_reupload_same_name_keeps_original() {
    let (_pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;

    // 首次上传 same.bin（内容 A）
    let content_a: Vec<u8> = vec![b'A'; 2048];
    upload_one_chunk(&app, &token, "reup-1", 0, 1, &content_a).await;
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/merge",
            r#"{"identifier":"reup-1","file_name":"same.bin","parent_path":""}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 同名重传（新 identifier）→ 409
    let content_b: Vec<u8> = vec![b'B'; 2048];
    upload_one_chunk(&app, &token, "reup-2", 0, 1, &content_b).await;
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/merge",
            r#"{"identifier":"reup-2","file_name":"same.bin","parent_path":""}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT, "同名重传应 409");

    // 磁盘内容必须仍是 A（修复前旧文件已被截断+删除）
    let disk = tokio::fs::read(format!("uploads/{}/same.bin", username))
        .await
        .unwrap();
    assert_eq!(disk, content_a, "旧文件内容不得被重传销毁");

    // 列表仍只有一条 same.bin
    let resp = app
        .clone()
        .oneshot(get_with_token("/api/files/list?path=", &token))
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let list: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let count = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["name"] == "same.bin")
        .count();
    assert_eq!(count, 1, "同名文件记录应只有一条");
}

/// 回归测试：中文目录（多字节）重命名/移动后，深层子路径不得损坏；
/// 名称含 % 的目录不得因 LIKE 通配符误伤其他行
#[tokio::test]
async fn test_rename_move_chinese_and_wildcard_subtree() {
    let (pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;

    // 目录树 目录/a%b + 深层文件 报告.txt
    for (name, parent) in [("目录", ""), ("a%b", "目录")] {
        let resp = app
            .clone()
            .oneshot(post_json_with_token(
                "/api/files/create_folder",
                &format!(r#"{{"name":"{}","current_path":"{}"}}"#, name, parent),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "创建 {}/{} 失败",
            parent,
            name
        );
    }
    let content: Vec<u8> = vec![b'X'; 512];
    upload_one_chunk(&app, &token, "zh-sub", 0, 1, &content).await;
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/merge",
            r#"{"identifier":"zh-sub","file_name":"报告.txt","parent_path":"目录/a%b"}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "上传深层文件失败");

    // 重命名 目录 → 新目录，深层子路径必须完整迁移
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/rename",
            r#"{"name":"目录","new_name":"新目录","current_path":"/"}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "中文目录重命名失败");

    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT name, parent_path FROM files WHERE username = ?")
            .bind(&username)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(
        rows.iter()
            .any(|(n, p)| n == "报告.txt" && p == "新目录/a%b"),
        "深层文件 parent_path 应为 新目录/a%b（多字节 SUBSTR 修复），实际: {:?}",
        rows
    );
    assert!(
        rows.iter().any(|(n, p)| n == "a%b" && p == "新目录"),
        "中间目录 a%b 的 parent_path 应为 新目录，实际: {:?}",
        rows
    );
    assert!(
        std::path::Path::new(&format!("uploads/{}/新目录/a%b/报告.txt", username)).exists(),
        "磁盘深层文件应随重命名迁移"
    );
}

/// 回归测试：外键约束必须在池中每条连接生效（连接选项级 PRAGMA）
#[tokio::test]
async fn test_foreign_keys_enforced() {
    let (pool, app) = test_app().await;

    // 引用不存在的用户 → FK 约束必须拒绝（v1.11 起用 todos 替代已移除的 conversations 表）
    let err = sqlx::query("INSERT INTO todos (username, title) VALUES ('ghost', '孤儿待办')")
        .execute(&pool)
        .await;
    assert!(err.is_err(), "引用不存在用户的待办应被 FK 拒绝");

    // 级联删除：删除用户后其待办应被 CASCADE 清除
    sqlx::query("INSERT INTO users (username, password, role) VALUES ('fkuser', 'x', 'user')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO todos (username, title) VALUES ('fkuser', 't1')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE username = 'fkuser'")
        .execute(&pool)
        .await
        .unwrap();
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM todos WHERE username = 'fkuser'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0, "删除用户应级联删除其待办");

    // 冒烟：应用路由仍可用
    let resp = app
        .clone()
        .oneshot(get_with_token("/health", ""))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// 媒体代理 Range 语义：HEAD 元数据 / 206 部分内容 / 416 越界 / 空文件 200
#[tokio::test]
async fn test_media_proxy_range_semantics() {
    let (pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;

    // 上传一个 1024 字节文件
    let content: Vec<u8> = (0..1024u32).map(|i| (i % 251) as u8).collect();
    upload_one_chunk(&app, &token, "sec-media-001", 0, 1, &content).await;
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/merge",
            r#"{"identifier":"sec-media-001","file_name":"range.bin","parent_path":""}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // HEAD → 元数据（Accept-Ranges + Content-Length）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("HEAD")
                .uri("/api/media/range.bin")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get("accept-ranges").unwrap(), "bytes");
    assert_eq!(resp.headers().get("content-length").unwrap(), "1024");

    // GET 带 Range → 206 + Content-Range
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/media/range.bin")
                .header("authorization", format!("Bearer {}", token))
                .header("range", "bytes=100-199")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert!(
        resp.headers()
            .get("content-range")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("bytes 100-199/1024")
    );
    let body = axum::body::to_bytes(resp.into_body(), 2048).await.unwrap();
    assert_eq!(body.len(), 100);
    assert_eq!(body[0], content[100]);
    assert_eq!(body[99], content[199]);

    // 越界 Range → 416
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/media/range.bin")
                .header("authorization", format!("Bearer {}", token))
                .header("range", "bytes=2000-3000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::RANGE_NOT_SATISFIABLE);

    // 空文件 → 200 空体（避免 Range 下溢）
    tokio::fs::write(format!("uploads/{}/empty.bin", username), b"")
        .await
        .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/media/empty.bin")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers().get("content-length").unwrap(), "0");

    // 清理
    eprintln!(
        "[DIAG] cwd={:?} uploads_exist={} file_exists={}",
        std::env::current_dir(),
        std::path::Path::new("uploads").exists(),
        std::path::Path::new(&format!("uploads/{}/range.bin", username)).exists()
    );
    let _ = tokio::fs::remove_file(format!("uploads/{}/range.bin", username)).await;
    let _ = tokio::fs::remove_file(format!("uploads/{}/empty.bin", username)).await;
    let _ = pool;
}

/// 消息端点:归属校验(他人对话 404)+ 空对话返回空数组

// ====== 10. WebDAV 测试 (v1.6.0) ======

fn basic_auth_header(username: &str, password: &str) -> String {
    use base64::Engine as _;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", username, password))
    )
}

async fn dav_send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    auth: Option<&str>,
    body: Vec<u8>,
    extra: &[(&str, &str)],
) -> axum::http::Response<axum::body::Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(a) = auth {
        builder = builder.header("authorization", a);
    }
    for (k, v) in extra {
        builder = builder.header(*k, *v);
    }
    let body = if body.is_empty() {
        Body::empty()
    } else {
        Body::from(body)
    };
    app.clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap()
}

async fn read_body_text(resp: axum::http::Response<axum::body::Body>) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).to_string()
}

/// 无凭据 PROPFIND → 401 + WWW-Authenticate
#[tokio::test]
async fn test_dav_propfind_requires_auth() {
    let (_pool, app) = test_app().await;
    let resp = dav_send(&app, "PROPFIND", "/dav/", None, vec![], &[("Depth", "1")]).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(
        resp.headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("Basic")),
        "必须返回 WWW-Authenticate: Basic"
    );
}

/// 错误密码 → 401
#[tokio::test]
async fn test_dav_wrong_password_rejected() {
    let (_pool, app) = test_app().await;
    let (_, username) = register_and_login_with_username(&app).await;
    let bad = basic_auth_header(&username, "wrong-password");
    let resp = dav_send(&app, "PROPFIND", "/dav/", Some(&bad), vec![], &[]).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// PUT → GET 回读一致；PROPFIND 列出子项
#[tokio::test]
async fn test_dav_put_get_roundtrip_and_propfind() {
    let (_pool, app) = test_app().await;
    let (_, username) = register_and_login_with_username(&app).await;
    let auth = basic_auth_header(&username, "testpass123");

    let resp = dav_send(
        &app,
        "PUT",
        "/dav/hello.txt",
        Some(&auth),
        b"hello dav".to_vec(),
        &[],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = dav_send(&app, "GET", "/dav/hello.txt", Some(&auth), vec![], &[]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(read_body_text(resp).await, "hello dav");

    // PROPFIND 根 Depth 1 → 包含 hello.txt 与配额属性
    let resp = dav_send(
        &app,
        "PROPFIND",
        "/dav/",
        Some(&auth),
        vec![],
        &[("Depth", "1")],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::MULTI_STATUS);
    let xml = read_body_text(resp).await;
    assert!(xml.contains("hello.txt"), "PROPFIND 应列出子项: {}", xml);
    assert!(xml.contains("quota-available-bytes"), "应含配额属性");

    // Depth infinity → 403
    let resp = dav_send(
        &app,
        "PROPFIND",
        "/dav/",
        Some(&auth),
        vec![],
        &[("Depth", "infinity")],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

/// PUT 覆盖 → 204 + 新内容生效
#[tokio::test]
async fn test_dav_put_overwrite() {
    let (_pool, app) = test_app().await;
    let (_, username) = register_and_login_with_username(&app).await;
    let auth = basic_auth_header(&username, "testpass123");

    dav_send(&app, "PUT", "/dav/a.txt", Some(&auth), b"v1".to_vec(), &[]).await;
    let resp = dav_send(
        &app,
        "PUT",
        "/dav/a.txt",
        Some(&auth),
        b"v2-longer".to_vec(),
        &[],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = dav_send(&app, "GET", "/dav/a.txt", Some(&auth), vec![], &[]).await;
    assert_eq!(read_body_text(resp).await, "v2-longer");
}

// ====== 10.1 MR-1 安全回归 (v1.8.1) ======

/// C1 回归：认证缓存必须绑定凭证指纹——正确密码认证成功后 60s 内，
/// 同用户名 + 错误密码必须仍然 401（历史实现仅按 username 放行）
#[tokio::test]
async fn test_dav_auth_cache_requires_correct_password() {
    let (_pool, app) = test_app().await;
    let (_, username) = register_and_login_with_username(&app).await;

    // 正确密码建立缓存
    let good = basic_auth_header(&username, "testpass123");
    let resp = dav_send(&app, "PROPFIND", "/dav/", Some(&good), vec![], &[]).await;
    assert_eq!(resp.status(), StatusCode::MULTI_STATUS);

    // 缓存窗口内：错误密码必须被拒绝
    let bad = basic_auth_header(&username, "completely-wrong");
    let resp = dav_send(&app, "PROPFIND", "/dav/", Some(&bad), vec![], &[]).await;
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "缓存命中不得仅凭用户名放行（C1）"
    );

    // 正确密码仍可通过（缓存未被错误尝试污染）
    let resp = dav_send(&app, "PROPFIND", "/dav/", Some(&good), vec![], &[]).await;
    assert_eq!(resp.status(), StatusCode::MULTI_STATUS);
}

/// H4 回归：WebDAV 认证尝试按对端 IP 限速，连续失败超过阈值 → 429
#[tokio::test]
async fn test_dav_brute_force_rate_limited() {
    let (_pool, app) = test_app().await;
    let (_, username) = register_and_login_with_username(&app).await;

    // 测试环境无 ConnectInfo → extract_ip 信任 x-forwarded-for；
    // 用进程内唯一 IP 隔离限速桶，避免与其他测试共享 "loopback" 键
    let ip = format!("10.99.{}.{}", std::process::id() % 250, 7);
    let bad = basic_auth_header(&username, "wrong-password");

    let mut got_429 = false;
    for _ in 0..(pi_nas::constants::LOGIN_RATE_LIMIT_ATTEMPTS + 1) {
        let resp = dav_send(
            &app,
            "PROPFIND",
            "/dav/",
            Some(&bad),
            vec![],
            &[("x-forwarded-for", &ip)],
        )
        .await;
        if resp.status() == StatusCode::TOO_MANY_REQUESTS {
            got_429 = true;
            break;
        }
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "限速阈值之前应为 401"
        );
    }
    assert!(got_429, "超过限速阈值后必须 429（H4）");
}

/// H3 回归：PUT 覆盖时内容策略失败（可执行 MIME）必须保留旧文件——
/// 历史实现先删旧文件再校验，403 时新旧两文件皆毁
#[tokio::test]
async fn test_dav_put_overwrite_blocked_content_preserves_old() {
    let (_pool, app) = test_app().await;
    let (_, username) = register_and_login_with_username(&app).await;
    let auth = basic_auth_header(&username, "testpass123");

    // 先正常写入一个文本文件
    let resp = dav_send(
        &app,
        "PUT",
        "/dav/notes.txt",
        Some(&auth),
        b"precious old content".to_vec(),
        &[],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // ELF magic（>52 字节）→ infer 判定 application/x-executable → 内容策略拒绝
    let mut exe = vec![0u8; 56];
    exe[0..4].copy_from_slice(&[0x7F, b'E', b'L', b'F']);
    let resp = dav_send(&app, "PUT", "/dav/notes.txt", Some(&auth), exe, &[]).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // 旧内容必须原封不动
    let resp = dav_send(&app, "GET", "/dav/notes.txt", Some(&auth), vec![], &[]).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        read_body_text(resp).await,
        "precious old content",
        "策略拒绝的覆盖不得销毁旧文件（H3）"
    );
}

/// H1 回归：bytes_received 必须累计（历史实现截断后才读旧大小，恒为 0）
/// 且同片重传不重复计数（5GB 上限核算依据）
#[tokio::test]
async fn test_chunk_bytes_received_accumulates() {
    let (pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;

    let identifier = format!("bytes-received-{}", std::process::id());
    let data = vec![0x61u8; CHUNK_SIZE];

    upload_one_chunk(&app, &token, &identifier, 0, 1, &data).await;

    let sum: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(bytes_received), 0) FROM upload_chunks WHERE username = ? AND identifier = ?",
    )
    .bind(&username)
    .bind(&identifier)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        sum as usize, CHUNK_SIZE,
        "bytes_received 应累计实际字节（H1）"
    );

    // 同片重传：计数保持单份（先减旧值再加新值）
    upload_one_chunk(&app, &token, &identifier, 0, 1, &data).await;
    let sum2: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(bytes_received), 0) FROM upload_chunks WHERE username = ? AND identifier = ?",
    )
    .bind(&username)
    .bind(&identifier)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(sum2 as usize, CHUNK_SIZE, "同片重传不得重复计数（H1）");
}

/// H2 回归：分片临时目录按用户隔离——用户 B 的 check 看不到 A 已上传的分片
#[tokio::test]
async fn test_chunk_dir_isolated_per_user() {
    let (_pool, app) = test_app().await;
    let (token_a, _u) = register_and_login_with_username(&app).await;
    let (token_b, _v) = register_and_login_with_username(&app).await;

    let identifier = format!("isolation-{}", std::process::id());
    let data = vec![0x62u8; CHUNK_SIZE];

    // A 上传分片 0
    upload_one_chunk(&app, &token_a, &identifier, 0, 2, &data).await;

    // A 自己能看到
    let resp = app
        .clone()
        .oneshot(get_with_token(
            &format!("/api/files/check?identifier={}", identifier),
            &token_a,
        ))
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 2048).await.unwrap();
    let check: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        check["uploaded_chunks"].as_array().unwrap().len(),
        1,
        "上传者应看到自己的分片"
    );

    // B 必须看不到（H2：目录按用户隔离）
    let resp = app
        .clone()
        .oneshot(get_with_token(
            &format!("/api/files/check?identifier={}", identifier),
            &token_b,
        ))
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 2048).await.unwrap();
    let check: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        check["uploaded_chunks"].as_array().unwrap().is_empty(),
        "其他用户不得看到他人分片（H2）"
    );
}

// ====== 10.2 MR-2 数据与集成回归 (v1.8.2) ======

/// M1 回归：rename 崩溃窗口重放——物理已改、DB 未动，启动重放必须补齐 DB
#[tokio::test]
async fn test_journal_replay_rename() {
    let (pool, app) = test_app().await;
    let (_, username) = register_and_login_with_username(&app).await;
    let auth = basic_auth_header(&username, "testpass123");

    dav_send(
        &app,
        "PUT",
        "/dav/docs/a.txt",
        Some(&auth),
        b"x".to_vec(),
        &[],
    )
    .await;

    // 模拟崩溃现场：物理已改名、DB 未动、意图日志仍在
    std::fs::rename(
        format!("uploads/{}/docs/a.txt", username),
        format!("uploads/{}/docs/b.txt", username),
    )
    .unwrap();
    sqlx::query("INSERT INTO fs_journal (username, op, src, dst) VALUES (?, 'rename', ?, ?)")
        .bind(&username)
        .bind("docs/a.txt")
        .bind("docs/b.txt")
        .execute(&pool)
        .await
        .unwrap();

    pi_nas::handlers::replay_fs_journal(&pool).await;

    let name: Option<String> =
        sqlx::query_scalar("SELECT name FROM files WHERE username = ? AND parent_path = 'docs'")
            .bind(&username)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(name.as_deref(), Some("b.txt"), "重放应把 DB 行改为新名");
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM fs_journal")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remaining, 0, "重放完成后意图日志应清空");
}

/// M1 回归：trash 崩溃窗口重放——物理已进回收站、DB 未动，重放必须补 trash 行 + 删 files 行
#[tokio::test]
async fn test_journal_replay_trash() {
    let (pool, app) = test_app().await;
    let (_, username) = register_and_login_with_username(&app).await;
    let auth = basic_auth_header(&username, "testpass123");

    dav_send(&app, "PUT", "/dav/t.txt", Some(&auth), b"x".to_vec(), &[]).await;

    let uuid = uuid::Uuid::new_v4().to_string();
    std::fs::create_dir_all("uploads/.trash").unwrap();
    std::fs::rename(
        format!("uploads/{}/t.txt", username),
        format!("uploads/.trash/{}", uuid),
    )
    .unwrap();
    sqlx::query("INSERT INTO fs_journal (username, op, src, dst) VALUES (?, 'trash', 't.txt', ?)")
        .bind(&username)
        .bind(&uuid)
        .execute(&pool)
        .await
        .unwrap();

    pi_nas::handlers::replay_fs_journal(&pool).await;

    let trash_row: Option<String> =
        sqlx::query_scalar("SELECT original_path FROM trash WHERE trash_uuid = ?")
            .bind(&uuid)
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert_eq!(trash_row.as_deref(), Some("t.txt"), "重放应补 trash 行");
    let files_cnt: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files WHERE username = ?")
        .bind(&username)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(files_cnt, 0, "重放应删除 files 行");
}

/// M5 回归：删除名为 a%b 的文件夹不得误删兄弟目录 aXb 的 DB 行
#[tokio::test]
async fn test_delete_wildcard_name_does_not_kill_siblings() {
    let (pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;

    // 两个兄弟文件夹：a%b 与 aXb（% 是 LIKE 通配符）
    for name in ["a%b", "aXb"] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/files/create_folder")
                    .header("authorization", format!("Bearer {}", token))
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"name":"{}","current_path":""}}"#,
                        name
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(resp.status().is_success(), "建夹失败: {}", name);
    }

    // 删除 a%b
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/files/delete")
                .header("authorization", format!("Bearer {}", token))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"name":"a%b","current_path":""}"#.to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "删除应成功");

    let sibling: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM files WHERE username = ? AND name = 'aXb'")
            .bind(&username)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(sibling, 1, "aXb 的 DB 行不得被误删（M5）");
}

/// M11 回归：并发同名建夹——恰一个成功、一个 409，目录与 DB 行都只有一份
#[tokio::test]
async fn test_concurrent_create_folder_same_name() {
    let (_pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;

    let req = |app: &axum::Router| {
        app.clone().oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/files/create_folder")
                .header("authorization", format!("Bearer {}", token))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"name":"race","current_path":""}"#.to_string(),
                ))
                .unwrap(),
        )
    };
    let (r1, r2) = tokio::join!(req(&app), req(&app));
    let (s1, s2) = (r1.unwrap().status(), r2.unwrap().status());
    let ok = i32::from(s1.is_success()) + i32::from(s2.is_success());
    let conflict = i32::from(s1 == StatusCode::CONFLICT) + i32::from(s2 == StatusCode::CONFLICT);
    assert_eq!((ok, conflict), (1, 1), "应恰一个成功一个 409");

    assert!(
        std::path::Path::new(&format!("uploads/{}/race", username)).is_dir(),
        "物理目录必须存在且只一份（M11）"
    );
    let rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM files WHERE username = ? AND name = 'race'")
            .bind(&username)
            .fetch_one(&_pool)
            .await
            .unwrap();
    assert_eq!(rows, 1, "DB 行必须恰一份（M11）");
}

/// M10 回归：超配额合并必须保留分片（清理空间后可重试）
#[tokio::test]
async fn test_merge_over_quota_keeps_chunks() {
    let (_pool, app) = test_app().await;
    let (token_admin, _a) = register_and_login_with_username(&app).await; // 首注册=admin
    let (token_b, username_b) = register_and_login_with_username(&app).await;

    let identifier = format!("quota-keep-{}", std::process::id());
    upload_one_chunk(&app, &token_b, &identifier, 0, 1, &vec![0x63u8; CHUNK_SIZE]).await;

    // admin 把 B 的配额压到 0 → 合并必然超配
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/quota")
                .header("authorization", format!("Bearer {}", token_admin))
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"username":"{}","quota_mb":0}}"#,
                    username_b
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_success(), "设置配额失败");

    // 合并 → 必须拒绝
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/files/merge")
                .header("authorization", format!("Bearer {}", token_b))
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"file_name":"out.bin","identifier":"{}","parent_path":"","total_chunks":1}}"#,
                    identifier
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(!resp.status().is_success(), "超配额合并必须被拒绝");

    // 分片必须还在（M10）
    assert!(
        std::path::Path::new(&format!("uploads/tmp/{}/{}/0", username_b, identifier)).is_file(),
        "超配额拒绝不得删除分片（M10）"
    );
}

/// M2 回归：zip 打包跳过符号链接（防递归环/越界打包）
#[tokio::test]
async fn test_zip_skips_symlinks() {
    let (_pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;
    let auth = basic_auth_header(&username, "testpass123");

    dav_send(
        &app,
        "PUT",
        "/dav/real.txt",
        Some(&auth),
        b"real".to_vec(),
        &[],
    )
    .await;
    // 指向目录外的符号链接
    std::os::unix::fs::symlink("/etc/passwd", format!("uploads/{}/evil.txt", username)).unwrap();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/files/download_zip")
                .header("authorization", format!("Bearer {}", token))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"names":["real.txt","evil.txt"],"current_path":""}"#.to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap();
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes.as_ref())).expect("zip 应可解析");
    let names: Vec<String> = (0..archive.len())
        .map(|i| archive.by_index(i).unwrap().name().to_string())
        .collect();
    assert!(names.iter().any(|n| n == "real.txt"), "常规文件应被打包");
    assert!(
        !names.iter().any(|n| n == "evil.txt"),
        "符号链接必须跳过（M2）"
    );
}

/// M3 回归：截断的分片必须在 merge 时被拒绝，不得产出损坏文件
#[tokio::test]
async fn test_merge_rejects_truncated_chunk() {
    let (_pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;

    let identifier = format!("trunc-{}", std::process::id());
    upload_one_chunk(&app, &token, &identifier, 0, 1, &vec![0x64u8; CHUNK_SIZE]).await;

    // 模拟崩溃截断：把分片文件改写为一半大小（check 会误报已传）
    std::fs::write(
        format!("uploads/tmp/{}/{}/0", username, identifier),
        vec![0x64u8; CHUNK_SIZE / 2],
    )
    .unwrap();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/files/merge")
                .header("authorization", format!("Bearer {}", token))
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"file_name":"out.bin","identifier":"{}","parent_path":"","total_chunks":1}}"#,
                    identifier
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "截断分片必须被拒绝");
    let body = axum::body::to_bytes(resp.into_body(), 2048).await.unwrap();
    assert!(
        String::from_utf8_lossy(&body).contains("损坏"),
        "错误信息应说明分片损坏"
    );
}

/// 首注册 admin 竞态回归：并发注册两个用户，必须恰一个 admin
/// （历史实现先 count 后 INSERT，并发可双双拿到 admin）
#[tokio::test]
async fn test_concurrent_first_registrations_single_admin() {
    let (pool, app) = test_app().await;
    let reg = |name: &str| {
        app.clone().oneshot(post_json(
            "/api/register",
            &format!(r#"{{"username":"{}","password":"testpass123"}}"#, name),
        ))
    };
    let (r1, r2) = tokio::join!(reg("race_admin_a"), reg("race_admin_b"));
    assert!(r1.unwrap().status().is_success(), "注册 A 应成功");
    assert!(r2.unwrap().status().is_success(), "注册 B 应成功");

    let admins: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE role = 'admin'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(admins, 1, "并发首注册只能产生一个 admin");
}

/// PUT 到不存在的子目录 → 目录行自动创建（ensure_dir_rows）
#[tokio::test]
async fn test_dav_put_creates_dir_rows() {
    let (pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;
    let auth = basic_auth_header(&username, "testpass123");

    let resp = dav_send(
        &app,
        "PUT",
        "/dav/sub/deep/file.txt",
        Some(&auth),
        b"nested".to_vec(),
        &[],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // 目录行已登记（sub 与 sub/deep）
    let cnt: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM files WHERE username = ? AND is_dir = 1 AND parent_path IN ('', 'sub')",
    )
    .bind(&username)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(cnt, 2, "sub 与 sub/deep 目录行应存在");

    // 全局搜索命中（FTS 触发器联动）——用同一用户的登录 token
    let resp = app
        .clone()
        .oneshot(get_with_token("/drive/list?path=&search=file", &token))
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let html = read_body_text(resp).await;
    assert!(html.contains("file.txt"), "全局搜索应命中: {}", html);
}

/// MOVE / COPY（含 Overwrite:F → 412）
#[tokio::test]
async fn test_dav_move_copy_overwrite() {
    let (_pool, app) = test_app().await;
    let (_, username) = register_and_login_with_username(&app).await;
    let auth = basic_auth_header(&username, "testpass123");

    dav_send(
        &app,
        "PUT",
        "/dav/src.txt",
        Some(&auth),
        b"move-me".to_vec(),
        &[],
    )
    .await;
    dav_send(
        &app,
        "PUT",
        "/dav/dst.txt",
        Some(&auth),
        b"occupied".to_vec(),
        &[],
    )
    .await;

    // Overwrite: F 且目标存在 → 412
    let resp = dav_send(
        &app,
        "MOVE",
        "/dav/src.txt",
        Some(&auth),
        vec![],
        &[("Destination", "/dav/dst.txt"), ("Overwrite", "F")],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::PRECONDITION_FAILED);

    // 正常 MOVE → 201；旧路径 404
    let resp = dav_send(
        &app,
        "MOVE",
        "/dav/src.txt",
        Some(&auth),
        vec![],
        &[("Destination", "/dav/moved.txt")],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp = dav_send(&app, "GET", "/dav/src.txt", Some(&auth), vec![], &[]).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let resp = dav_send(&app, "GET", "/dav/moved.txt", Some(&auth), vec![], &[]).await;
    let st = resp.status();
    let cl = resp
        .headers()
        .get("content-length")
        .map(|v| v.to_str().unwrap_or("?").to_string());
    let bd = read_body_text(resp).await;
    println!("[TEST] GET moved status={} cl={:?} body={:?}", st, cl, bd);

    // COPY → 源保留 + 副本存在
    let resp = dav_send(
        &app,
        "COPY",
        "/dav/moved.txt",
        Some(&auth),
        vec![],
        &[("Destination", "/dav/copied.txt")],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp = dav_send(&app, "GET", "/dav/copied.txt", Some(&auth), vec![], &[]).await;
    assert_eq!(read_body_text(resp).await, "move-me");
    let resp = dav_send(&app, "GET", "/dav/moved.txt", Some(&auth), vec![], &[]).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

/// DELETE → 进回收站（可还原）+ 配额回落
#[tokio::test]
async fn test_dav_delete_goes_to_trash() {
    let (pool, app) = test_app().await;
    let (_, username) = register_and_login_with_username(&app).await;
    let auth = basic_auth_header(&username, "testpass123");

    dav_send(
        &app,
        "PUT",
        "/dav/trashme.txt",
        Some(&auth),
        b"bye".to_vec(),
        &[],
    )
    .await;
    let resp = dav_send(&app, "DELETE", "/dav/trashme.txt", Some(&auth), vec![], &[]).await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = dav_send(&app, "GET", "/dav/trashme.txt", Some(&auth), vec![], &[]).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    // 回收站有记录（可还原）
    let cnt: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM trash WHERE username = ? AND original_path = 'trashme.txt'",
    )
    .bind(&username)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(cnt, 1, "删除应进回收站");
}

/// Range GET → 206 + 正确 Content-Range
#[tokio::test]
async fn test_dav_range_get() {
    let (_pool, app) = test_app().await;
    let (_, username) = register_and_login_with_username(&app).await;
    let auth = basic_auth_header(&username, "testpass123");

    dav_send(
        &app,
        "PUT",
        "/dav/range.bin",
        Some(&auth),
        b"0123456789".to_vec(),
        &[],
    )
    .await;
    let resp = dav_send(
        &app,
        "GET",
        "/dav/range.bin",
        Some(&auth),
        vec![],
        &[("Range", "bytes=2-5")],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        resp.headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok()),
        Some("bytes 2-5/10")
    );
    assert_eq!(read_body_text(resp).await, "2345");
}

/// MKCOL 重复 → 405；配额耗尽 PUT → 507
#[tokio::test]
async fn test_dav_mkcol_and_quota() {
    let (pool, app) = test_app().await;
    let (_, username) = register_and_login_with_username(&app).await;
    let auth = basic_auth_header(&username, "testpass123");

    let resp = dav_send(&app, "MKCOL", "/dav/newdir", Some(&auth), vec![], &[]).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp = dav_send(&app, "MKCOL", "/dav/newdir", Some(&auth), vec![], &[]).await;
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);

    // 配额压到 1MB，PUT 2MB → 507
    sqlx::query("UPDATE users SET quota_mb = 1 WHERE username = ?")
        .bind(&username)
        .execute(&pool)
        .await
        .unwrap();
    let big = vec![b'x'; 2 * 1024 * 1024];
    let resp = dav_send(&app, "PUT", "/dav/big.bin", Some(&auth), big, &[]).await;
    assert_eq!(resp.status(), StatusCode::INSUFFICIENT_STORAGE);
    // 目标不得出现
    let resp = dav_send(&app, "GET", "/dav/big.bin", Some(&auth), vec![], &[]).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// 中文搜索：3 字走 FTS、2 字降级 LIKE 均命中
#[tokio::test]
async fn test_search_global_cjk_and_ascii() {
    let (_pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;
    let auth = basic_auth_header(&username, "testpass123");

    dav_send(
        &app,
        "PUT",
        "/dav/%E4%BD%BF%E7%94%A8%E6%89%8B%E5%86%8C.pdf",
        Some(&auth),
        b"%PDF".to_vec(),
        &[],
    )
    .await; // 使用手册.pdf
    dav_send(
        &app,
        "PUT",
        "/dav/report.docx",
        Some(&auth),
        b"x".to_vec(),
        &[],
    )
    .await;

    // 3 字中文词（FTS trigram）
    let resp = app
        .clone()
        .oneshot(get_with_token(
            "/drive/list?path=&search=%E6%89%8B%E5%86%8C",
            &token,
        ))
        .await
        .unwrap();
    assert!(resp.status().is_success());
    assert!(
        read_body_text(resp).await.contains("使用手册"),
        "3 字中文词应命中"
    );
    // 2 字中文词（LIKE 兜底）
    let resp = app
        .clone()
        .oneshot(get_with_token(
            "/drive/list?path=&search=%E4%BD%BF%E7%94%A8",
            &token,
        ))
        .await
        .unwrap();
    assert!(
        read_body_text(resp).await.contains("使用手册"),
        "2 字词 LIKE 兜底应命中"
    );
    // ASCII 子串
    let resp = app
        .clone()
        .oneshot(get_with_token("/drive/list?path=&search=report", &token))
        .await
        .unwrap();
    assert!(read_body_text(resp).await.contains("report.docx"));
}

/// merge 到未登记子目录 → 目录行自动补插（文件夹上传支撑）
#[tokio::test]
async fn test_merge_to_nested_dir_creates_rows() {
    let (pool, app) = test_app().await;
    let (token, _) = register_and_login_with_username(&app).await;
    upload_one_chunk(&app, &token, "nested-001", 0, 1, b"nested file").await;
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/merge",
            r#"{"identifier":"nested-001","file_name":"f.txt","parent_path":"a/b"}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "merge 到子目录应成功");
    let cnt: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM files WHERE is_dir = 1 AND parent_path = ''")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(cnt, 1, "a 目录行应存在");
    let cnt: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM files WHERE is_dir = 1 AND parent_path = 'a'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(cnt, 1, "a/b 目录行应存在");
}

/// markdown 预览走渲染分支（含 JSON 数据块）
#[tokio::test]
async fn test_preview_markdown_mode() {
    let (_pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;
    let auth = basic_auth_header(&username, "testpass123");
    dav_send(
        &app,
        "PUT",
        "/dav/readme.md",
        Some(&auth),
        b"# Title\n\n**bold** <script>alert(1)</script>".to_vec(),
        &[],
    )
    .await;
    let resp = app
        .clone()
        .oneshot(get_with_token(
            "/drive/preview?path=%2F&name=readme.md",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = read_body_text(resp).await;
    assert!(
        html.contains("markdown-data"),
        "markdown 模式应输出数据块: {}",
        html
    );
    assert!(
        html.contains("\\u003cscript"),
        "script 标签必须转义防 </script> 逃逸: {}",
        html
    );
}

// ====== 11. P1 批次回归测试 (v1.7.0) ======

/// F9: 分片空洞（缺中间片）合并必须拒绝，而非产出错位损坏文件
#[tokio::test]
async fn test_merge_rejects_gap() {
    let (_pool, app) = test_app().await;
    let (token, _) = register_and_login_with_username(&app).await;

    let identifier = "gap-test-1";
    let data = vec![0xAB; 1024];
    // 上传 0,1,3 共 3 片（总数声明 4，缺 2）
    for idx in [0, 1, 3] {
        upload_one_chunk(&app, &token, identifier, idx, 4, &data).await;
    }
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/merge",
            &format!(
                r#"{{"identifier":"{}","file_name":"gap.bin","parent_path":""}}"#,
                identifier
            ),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "分片空洞必须拒绝");
    let body = axum::body::to_bytes(resp.into_body(), 2048).await.unwrap();
    assert!(
        String::from_utf8_lossy(&body).contains("分片不完整"),
        "应返回分片不完整提示"
    );
}

/// F12: 带时间的日程必须出现在日历与日期筛选中
#[tokio::test]
async fn test_timed_todo_in_calendar_range() {
    let (pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;

    // 直接 SQL 插入带时间的日程（含 T 分隔）
    sqlx::query(
        "INSERT INTO todos (username, title, due_date, is_all_day, category) VALUES (?, '定时日程', '2026-08-13T10:00:00', 0, 'schedule')",
    )
    .bind(&username)
    .execute(&pool)
    .await
    .unwrap();

    // 日历格子按日期统计（cell 显示 count 而非标题），断言 8 月 13 日格有计数
    let resp = app
        .clone()
        .oneshot(get_with_token("/todos/calendar?year=2026&month=8", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
    let html = String::from_utf8_lossy(&body).to_string();
    // 有日程的格子才渲染 hx-get="/todos/list?date=..." 链接——13 日必须带（修复前
    // 带时间日程被月份范围查询漏掉，13 日格为空）
    assert!(
        html.contains("date=2026-08-13"),
        "13 日格应包含日程（日历格子带日期链接）"
    );

    // /todos/list 日期筛选（日历点击路径）应列出该日程标题
    let resp = app
        .clone()
        .oneshot(get_with_token("/todos/list?date=2026-08-13", &token))
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
    let list_html = String::from_utf8_lossy(&body).to_string();
    assert!(
        list_html.contains("定时日程"),
        "/todos/list?date= 应列出带时间日程"
    );

    // 精确日期筛选应包含
    let resp = app
        .clone()
        .oneshot(get_with_token("/api/todos?date=2026-08-13", &token))
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
    let todos: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        todos.as_array().unwrap().len(),
        1,
        "date=2026-08-13 应命中带时间日程"
    );
}

/// F16: 媒体令牌 — 签发、路径限定、过期拒绝；旧 ?token= 会话凭证不再接受
#[tokio::test]
async fn test_media_token_scoped_and_expires() {
    let (pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;

    // 子目录 sub 下上传一个 PNG（合法 MIME 头）
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/create_folder",
            r#"{"name":"sub","current_path":"/"}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let png: Vec<u8> = {
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        v.extend_from_slice(&[0u8; 64]);
        v
    };
    upload_one_chunk(&app, &token, "mt-png", 0, 1, &png).await;
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/merge",
            r#"{"identifier":"mt-png","file_name":"pic.png","parent_path":"sub"}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 预览片段应含 ?mt= 媒体令牌
    let resp = app
        .clone()
        .oneshot(get_with_token(
            "/drive/preview?path=/sub&name=pic.png",
            &token,
        ))
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
    let html = String::from_utf8_lossy(&body).to_string();
    let mt_pos = html.find("?mt=").expect("预览应包含媒体令牌");
    let mt = html[mt_pos + 4..]
        .split(['"', '&', '#'])
        .next()
        .unwrap()
        .to_string();
    assert!(!mt.is_empty());

    // 无 Cookie/Authorization，仅 mt → 200
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/media/sub/pic.png?mt={}", mt))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "合法媒体令牌应放行");

    // 路径限定：令牌签发于 sub/ 目录，访问根目录其他文件必须拒绝
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/media/{}/sub/pic.png?mt={}", username, mt))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "路径前缀之外的访问必须拒绝"
    );

    // 旧 ?token= 会话凭证不再接受
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/media/sub/pic.png?token={}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "会话 token 走 URL 查询串必须被拒绝"
    );

    // 过期令牌拒绝
    sqlx::query("UPDATE media_tokens SET expires_at = datetime('now', '-1 hour')")
        .execute(&pool)
        .await
        .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/media/sub/pic.png?mt={}", mt))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "过期令牌必须拒绝");
}

/// F17: 分享密码连续失败锁定（5 次错误 → 429，锁定 15 分钟）
#[tokio::test]
async fn test_share_bruteforce_lockout() {
    let (_pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;

    // 上传文件并创建带密码分享
    let data = vec![0x11; 1024];
    upload_one_chunk(&app, &token, "lockout-f", 0, 1, &data).await;
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/merge",
            r#"{"identifier":"lockout-f","file_name":"lock.bin","parent_path":""}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/share/create",
            r#"{"file_path":"lock.bin","is_dir":false,"password":"right-pass"}"#,
            &token,
        ))
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let share: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let code = share["code"].as_str().unwrap().to_string();

    // 连续 5 次错误密码 → 401；第 6 次 → 429 锁定
    for i in 1..=5 {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/share/access/{}?password=wrong-{}", code, i))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "第 {} 次错误应 401",
            i
        );
    }
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/share/access/{}?password=wrong-6", code))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "连续失败应锁定"
    );

    // 正确密码也被锁定期拒绝（锁定的是分享而非密码）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/share/access/{}?password=right-pass", code))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);

    // 清理测试文件
    let _ = tokio::fs::remove_file(format!("uploads/{}/lock.bin", username)).await;
}

/// F21: javascript: 等危险 scheme 链接必须被拒绝（JSON 与 HTMX 片段路径一致）
#[tokio::test]
async fn test_links_reject_javascript_url() {
    let (pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;

    // JSON API → 400
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/links",
            r#"{"title":"evil","url":"javascript:alert(document.cookie)"}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "javascript: URL 应 400"
    );

    // HTMX 片段 → 不落库（回空表单）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/links")
                .header("authorization", format!("Bearer {}", token))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=evil&url=javascript%3Aalert%281%29"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM links WHERE username = ?")
        .bind(&username)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0, "危险 URL 不得落库");

    // data: URL 同样拒绝
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/links",
            r#"{"title":"evil2","url":"data:text/html,<script>alert(1)</script>"}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "data: URL 应 400");
}

/// F15: Cookie 默认强制 Secure（无 X-Forwarded-Proto 也带 Secure 标志）
#[tokio::test]
async fn test_login_cookie_always_secure() {
    let (_pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;
    // 登出后重新登录，检查 Set-Cookie
    let _ = app
        .clone()
        .oneshot(post_json_with_token("/api/logout", "{}", &token))
        .await
        .unwrap();
    let resp = app
        .clone()
        .oneshot(post_json(
            "/api/login",
            &format!(r#"{{"username":"{}","password":"testpass123"}}"#, username),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let set_cookie = resp
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap_or_default().to_string())
        .unwrap_or_default();
    assert!(
        set_cookie.contains("Secure"),
        "Cookie 应默认带 Secure 标志: {}",
        set_cookie
    );
}

/// F20: 未知用户登录返回 401（哑哈希等时校验不改变响应语义）
#[tokio::test]
async fn test_login_unknown_user_delayed() {
    let (_pool, app) = test_app().await;
    let resp = app
        .clone()
        .oneshot(post_json(
            "/api/login",
            r#"{"username":"no-such-user","password":"whatever123"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// F26: 全局搜索模式下排序表头保留 search 参数
#[tokio::test]
async fn test_global_search_sort_keeps_query() {
    let (_pool, app) = test_app().await;
    let (token, _) = register_and_login_with_username(&app).await;
    // 全局搜索模式要求 path 显式为 ""（缺省会归一为 "/"）
    let resp = app
        .clone()
        .oneshot(get_with_token(
            "/drive/list?path=&search=readme&sort_by=size_desc",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
    let html = String::from_utf8_lossy(&body).to_string();
    // Askama 对属性内插值做 HTML 转义（&#34;/&quot;）：浏览器 getAttribute 会还原实体
    let normalized = html.replace("&#34;", "\"").replace("&quot;", "\"");
    assert!(
        normalized.contains(r#""search":"readme""#),
        "排序表头 hx-vals 应保留 search 参数"
    );
}

/// F27: page_size 非正数必须钳位（负 LIMIT 在 SQLite 意为无限制）
#[tokio::test]
async fn test_list_page_size_clamped() {
    let (_pool, app) = test_app().await;
    let (token, _) = register_and_login_with_username(&app).await;
    for ps in ["0", "-5"] {
        let resp = app
            .clone()
            .oneshot(get_with_token(
                &format!("/api/files/list?path=&page_size={}", ps),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "page_size={} 应 200", ps);
    }
}

/// F10: WebDAV MOVE 覆盖失败后，被位移目标的 DB 行必须完整恢复（不"隐身"）
#[tokio::test]
async fn test_dav_move_failure_restores_displaced_rows() {
    let (pool, app) = test_app().await;
    let (_token, username) = register_and_login_with_username(&app).await;
    let auth_header = basic_auth_header(&username, "testpass123");
    let auth = Some(auth_header.as_str());

    // PUT 目标文件 b.txt（真实存在）
    let resp = dav_send(&app, "PUT", "/dav/b.txt", auth, b"B-data".to_vec(), &[]).await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // MOVE 一个不存在的源文件覆盖 b.txt：位移 b.txt 后 move 失败 → 必须完整回滚
    let resp = dav_send(
        &app,
        "MOVE",
        "/dav/ghost.txt",
        auth,
        Vec::new(),
        &[
            ("Destination", "http://localhost/dav/b.txt"),
            ("Overwrite", "T"),
        ],
    )
    .await;
    assert!(
        resp.status().is_client_error() || resp.status().is_server_error(),
        "移动不存在的源应失败"
    );

    // b.txt 的 DB 行必须仍在（修复前被删 → 文件"隐身"）
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM files WHERE username = ? AND name = 'b.txt' AND parent_path = ''",
    )
    .bind(&username)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n, 1, "失败回滚后 b.txt 的 DB 行必须恢复");

    // 物理文件也在
    assert!(
        tokio::fs::try_exists(format!("uploads/{}/b.txt", username))
            .await
            .unwrap(),
        "b.txt 物理文件必须恢复"
    );

    // 清理
    let _ = tokio::fs::remove_file(format!("uploads/{}/b.txt", username)).await;
}

// ====== 12. P2 批次回归测试 (v1.7.1) ======

/// F32: 全局搜索大小写不敏感（v9 迁移重建 trigram case_sensitive 0）
#[tokio::test]
async fn test_global_search_case_insensitive() {
    let (_pool, app) = test_app().await;
    let (token, _) = register_and_login_with_username(&app).await;

    // 上传 README.md（大写），搜索小写 readme 应命中
    let data = b"hello world".to_vec();
    upload_one_chunk(&app, &token, "fts-case", 0, 1, &data).await;
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/merge",
            r#"{"identifier":"fts-case","file_name":"README.md","parent_path":""}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .clone()
        .oneshot(get_with_token("/api/files/list?search=readme", &token))
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
    let list: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let found = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["name"] == "README.md");
    assert!(found, "小写搜索应命中大写文件名（case_sensitive 0）");
}

/// F39: HSTS 仅在 HTTPS 接入时下发
#[tokio::test]
async fn test_hsts_only_under_https() {
    let (_pool, app) = test_app().await;
    let (token, _) = register_and_login_with_username(&app).await;

    // 无 X-Forwarded-Proto → 无 HSTS
    let resp = app
        .clone()
        .oneshot(get_with_token("/api/files/list?path=", &token))
        .await
        .unwrap();
    assert!(
        resp.headers().get("strict-transport-security").is_none(),
        "纯 HTTP 接入不应下发 HSTS"
    );

    // 带 X-Forwarded-Proto: https → 有 HSTS
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/files/list?path=")
                .header("authorization", format!("Bearer {}", token))
                .header("x-forwarded-proto", "https")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.headers().get("strict-transport-security").is_some(),
        "HTTPS 接入应下发 HSTS"
    );
}

/// F16 顺带回归：预览媒体令牌过期/越界后，cookie 会话仍可正常访问媒体
#[tokio::test]
async fn test_media_still_works_with_session_auth() {
    let (_pool, app) = test_app().await;
    let (token, _) = register_and_login_with_username(&app).await;

    let data = b"\x89PNG\r\n\x1a\n".to_vec();
    upload_one_chunk(&app, &token, "media-sess", 0, 1, &data).await;
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/merge",
            r#"{"identifier":"media-sess","file_name":"sess.png","parent_path":""}"#,
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Bearer 会话凭证仍正常（媒体令牌是补充通道，不是替代）
    let resp = app
        .clone()
        .oneshot(get_with_token("/api/media/sess.png", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// v1.7.3: 账号设置弹出式改密片段——登录用户 200 + 表单内容；未登录跳登录页
#[tokio::test]
async fn test_password_form_fragment() {
    let (_pool, app) = test_app().await;
    let (token, _) = register_and_login_with_username(&app).await;

    let resp = app
        .clone()
        .oneshot(get_with_token("/account/password-form", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
    let html = String::from_utf8_lossy(&body).to_string();
    assert!(
        html.contains("pwd-modal-form"),
        "弹窗表单应包含修改密码表单"
    );

    // 未登录 → 重定向到登录页（非 API 路径语义）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/account/password-form")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        resp.status().is_redirection(),
        "未登录应重定向，实际: {}",
        resp.status()
    );
}

// ====== dsh 反代（统一登录 + HTTP/WS 转发） ======

/// 极简 mock 上游：接收一个请求，记录原始请求文本，返回 "OK host=<Host头>"
async fn mock_dsh_upstream(seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        if let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = [0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let req_str = String::from_utf8_lossy(&buf[..n]).to_string();
            seen.lock().unwrap().push(req_str.clone());
            let host = req_str
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .starts_with("host: ")
                        .then(|| l.trim_start_matches("host: ").to_string())
                })
                .unwrap_or_default();
            let body = format!("OK host={}", host);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        }
    });
    addr.to_string()
}

/// 配置 cookie_domain 时 Set-Cookie 携带 Domain 属性；默认不携带
#[tokio::test]
async fn test_cookie_domain_flag() {
    // 配置了 Domain
    let config = Config {
        cookie_domain: Some("antifield.work".into()),
        ..Default::default()
    };
    let (_pool, app) = test_app_with_config(config).await;
    let (token, _) = register_and_login_with_username(&app).await;
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/logout")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let sc = resp.headers().get("set-cookie").unwrap().to_str().unwrap();
    assert!(
        sc.contains("Domain=antifield.work"),
        "配置 cookie_domain 后 Set-Cookie 应含 Domain: {}",
        sc
    );

    // 默认配置不含 Domain（host-only cookie 语义不变）
    let (_pool, app) = test_app_with_config(Config::default()).await;
    let (token, _) = register_and_login_with_username(&app).await;
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/logout")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let sc = resp.headers().get("set-cookie").unwrap().to_str().unwrap();
    assert!(
        !sc.contains("Domain="),
        "默认配置 Set-Cookie 不应含 Domain: {}",
        sc
    );
}

/// 未登录访问 dsh 反代：导航 302 到 drive 绝对登录地址，API 401 JSON
#[tokio::test]
async fn test_dsh_gate_unauthenticated() {
    ensure_test_cwd();
    let pool = test_pool().await;
    db::init_test_db(&pool).await.unwrap();
    let config = Config {
        dsh_public_host: Some("dsh.antifield.work".into()),
        drive_public_url: Some("https://drive.antifield.work".into()),
        dsh_upstream_url: "http://127.0.0.1:1".into(),
        ..Default::default()
    };
    let app = router::build_dsh_router(config, pool);

    // 导航请求 → 302 绝对 drive 登录地址（redirect 回 dsh）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let loc = resp.headers().get("location").unwrap().to_str().unwrap();
    // M12：redirect 参数 percent-encode——路径与查询串不得裸拼进 Location
    assert_eq!(
        loc,
        "https://drive.antifield.work/login?redirect=https://dsh.antifield.work%2F"
    );

    // 带查询串的路径：& 必须被编码，否则 drive 侧 URLSearchParams 解析截断
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/docs?x=1&y=2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let loc = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(
        loc,
        "https://drive.antifield.work/login?redirect=https://dsh.antifield.work%2Fdocs%3Fx%3D1%26y%3D2",
        "查询串中的 & 必须编码（M12）"
    );

    // API 请求（Accept json）→ 401 JSON
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/foo")
                .header("accept", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"], "unauthorized");
}

/// 已登录请求：路径/查询透传 + Host 头注入为 dsh 公网主机名（信任栅栏要求）
#[tokio::test]
async fn test_dsh_proxy_forwards_and_injects_host() {
    ensure_test_cwd();
    let pool = test_pool().await;
    db::init_test_db(&pool).await.unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let upstream = mock_dsh_upstream(seen.clone()).await;
    let config = Config {
        dsh_upstream_url: format!("http://{}", upstream),
        dsh_public_host: Some("dsh.antifield.work".into()),
        ..Default::default()
    };
    let app = router::build_dsh_router(config, pool.clone());
    let main_app = router::build_router(Config::default(), pool);
    let (token, _) = register_and_login_with_username(&main_app).await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/assets/index.js?rev=1")
                .header("authorization", format!("Bearer {}", token))
                .header("cookie", format!("auth_token={}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    assert!(
        String::from_utf8_lossy(&body).starts_with("OK host="),
        "mock 响应: {}",
        String::from_utf8_lossy(&body)
    );

    let seen = seen.lock().unwrap();
    let req_str = seen.first().expect("mock 应收到请求");
    assert!(
        req_str.starts_with("GET /assets/index.js?rev=1"),
        "路径与查询应透传: {}",
        req_str
    );
    assert!(
        req_str
            .to_ascii_lowercase()
            .contains("host: dsh.antifield.work"),
        "Host 头应注入为公网主机名: {}",
        req_str
    );
    // H6 回归：pinas 会话凭据不得跨进程边界透传
    let lower = req_str.to_ascii_lowercase();
    assert!(
        !lower.contains("authorization:"),
        "Authorization 头不得转发上游（H6）: {}",
        req_str
    );
    assert!(
        !lower.contains("cookie:"),
        "Cookie 头不得转发上游（H6）: {}",
        req_str
    );
}

/// 上游不可达：API 502 JSON，导航 502 HTML
#[tokio::test]
async fn test_dsh_proxy_bad_gateway() {
    ensure_test_cwd();
    let pool = test_pool().await;
    db::init_test_db(&pool).await.unwrap();
    let config = Config {
        dsh_upstream_url: "http://127.0.0.1:1".into(),
        dsh_public_host: Some("dsh.antifield.work".into()),
        ..Default::default()
    };
    let app = router::build_dsh_router(config, pool.clone());
    let main_app = router::build_router(Config::default(), pool);
    let (token, _) = register_and_login_with_username(&main_app).await;

    // API → 502 JSON
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/foo")
                .header("authorization", format!("Bearer {}", token))
                .header("accept", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);

    // 导航 → 502 HTML
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
}

/// dsh 反代仅 admin 可见：普通用户 403，admin 200
#[tokio::test]
async fn test_dsh_admin_only() {
    ensure_test_cwd();
    let pool = test_pool().await;
    db::init_test_db(&pool).await.unwrap();
    let upstream = mock_dsh_upstream(std::sync::Arc::new(std::sync::Mutex::new(Vec::new()))).await;
    let config = Config {
        dsh_upstream_url: format!("http://{}", upstream),
        dsh_public_host: Some("pidsh.antifield.work".into()),
        ..Default::default()
    };
    let app = router::build_dsh_router(config, pool.clone());
    let main_app = router::build_router(Config::default(), pool.clone());
    let (user_token, username) = register_and_login_with_username(&main_app).await;
    // 注册的首个用户自动为 admin，显式降为普通用户再断言
    sqlx::query("UPDATE users SET role = 'user' WHERE username = ?")
        .bind(&username)
        .execute(&pool)
        .await
        .unwrap();

    // 普通用户 → 403
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header("authorization", format!("Bearer {}", user_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // 提升为 admin → 200（代理到达 mock 上游）
    sqlx::query("UPDATE users SET role = 'admin' WHERE username = ?")
        .bind(&username)
        .execute(&pool)
        .await
        .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header("authorization", format!("Bearer {}", user_token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// M3/M4 回归：dav PUT 超配额必须 507 且旧文件原样保留（配额事务原子化后）
#[tokio::test]
async fn test_dav_put_over_quota_preserves_old() {
    let (pool, app) = test_app().await;
    let (token_admin, _a) = register_and_login_with_username(&app).await; // 首注册=admin
    let (_, username_b) = register_and_login_with_username(&app).await;
    let auth = basic_auth_header(&username_b, "testpass123");

    dav_send(
        &app,
        "PUT",
        "/dav/keep.txt",
        Some(&auth),
        b"precious".to_vec(),
        &[],
    )
    .await;

    // admin 把 B 配额压到 0
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/quota")
                .header("authorization", format!("Bearer {}", token_admin))
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"username":"{}","quota_mb":0}}"#,
                    username_b
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // 覆盖 → 507（即使覆盖是等大替换，历史实现预检放行后仍可能 507；断言旧内容仍在）
    let resp = dav_send(
        &app,
        "PUT",
        "/dav/keep.txt",
        Some(&auth),
        b"new-content".to_vec(),
        &[],
    )
    .await;
    assert_eq!(resp.status(), StatusCode::INSUFFICIENT_STORAGE);
    let resp = dav_send(&app, "GET", "/dav/keep.txt", Some(&auth), vec![], &[]).await;
    assert_eq!(
        read_body_text(resp).await,
        "precious",
        "超配拒绝不得损坏旧文件"
    );
    let _ = pool;
}

/// M6 回归：PROPFIND getcontentlength 必须是真实字节（非 size_mb 反算）
#[tokio::test]
async fn test_propfind_exact_content_length() {
    let (_, app) = test_app().await;
    let (_, username) = register_and_login_with_username(&app).await;
    let auth = basic_auth_header(&username, "testpass123");
    // 5 字节文件：size_mb 反算会放大到 1048576
    dav_send(
        &app,
        "PUT",
        "/dav/five.txt",
        Some(&auth),
        b"hello".to_vec(),
        &[],
    )
    .await;
    let resp = dav_send(
        &app,
        "PROPFIND",
        "/dav/five.txt",
        Some(&auth),
        vec![],
        &[("Depth", "0")],
    )
    .await;
    let xml = read_body_text(resp).await;
    assert!(
        xml.contains("<D:getcontentlength>5</D:getcontentlength>"),
        "应为真实字节 5: {}",
        xml
    );
}

/// L5 回归：同内容不同目标路径不得误报秒传（目标文件必须真实存在过）
#[tokio::test]
async fn test_instant_upload_requires_same_target() {
    let (_pool, app) = test_app().await;
    let (token, _u) = register_and_login_with_username(&app).await;
    let identifier = format!("inst-{}", std::process::id());
    let data = vec![0x65u8; CHUNK_SIZE];
    upload_one_chunk(&app, &token, &identifier, 0, 1, &data).await;
    // merge 到 out.bin
    let resp = app.clone().oneshot(
        Request::builder().method("POST").uri("/api/files/merge")
            .header("authorization", format!("Bearer {}", token))
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"file_name":"out.bin","identifier":"{}","parent_path":"","total_chunks":1}}"#, identifier)))
            .unwrap(),
    ).await.unwrap();
    assert!(resp.status().is_success());

    // 同路径 → exists:true
    let resp = app
        .clone()
        .oneshot(get_with_token(
            &format!(
                "/api/files/check?identifier={}&file_name=out.bin&parent_path=",
                identifier
            ),
            &token,
        ))
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 2048).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["exists"].as_bool().unwrap(), "同路径应命中秒传");

    // 不同文件名 → exists:false（不得假秒传）
    let resp = app
        .clone()
        .oneshot(get_with_token(
            &format!(
                "/api/files/check?identifier={}&file_name=other.bin&parent_path=",
                identifier
            ),
            &token,
        ))
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 2048).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(!v["exists"].as_bool().unwrap(), "不同路径不得假秒传（L5）");
}

/// L8 回归：改密后旧凭证不得命中 dav 认证缓存
#[tokio::test]
async fn test_dav_cache_invalidated_on_password_change() {
    let (_, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;
    let old_auth = basic_auth_header(&username, "testpass123");

    // 建立缓存
    let resp = dav_send(&app, "PROPFIND", "/dav/", Some(&old_auth), vec![], &[]).await;
    assert_eq!(resp.status(), StatusCode::MULTI_STATUS);

    // 改密
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/user/password")
                .header("authorization", format!("Bearer {}", token))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"current_password":"testpass123","new_password":"newpass456"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 旧密码立即 401（缓存已失效）
    let resp = dav_send(&app, "PROPFIND", "/dav/", Some(&old_auth), vec![], &[]).await;
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "改密后旧凭证必须立即失效（L8）"
    );
}

/// api_base 深度校验回归（写入侧）

/// CSP 回归：script-src 无 unsafe-inline + 含主题预涂哈希；页面/片段无内联事件处理器
#[tokio::test]
async fn test_csp_and_templates_no_inline_handlers() {
    let (_pool, app) = test_app().await;
    let (token, _u) = register_and_login_with_username(&app).await;

    // 登录页（公开）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let csp = resp
        .headers()
        .get("content-security-policy")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(csp.contains("script-src"), "缺 script-src");
    let script_src = csp.split(';').find(|d| d.contains("script-src")).unwrap();
    assert!(
        !script_src.contains("unsafe-inline"),
        "script-src 不得含 unsafe-inline: {}",
        script_src
    );
    assert!(
        script_src.contains("sha256-"),
        "预涂脚本需哈希放行: {}",
        script_src
    );
    let html = read_body_text(resp).await;
    for attr in [
        "onclick=",
        "onsubmit=",
        "onchange=",
        "oninput=",
        "onerror=",
        "onload=",
    ] {
        assert!(
            !html.contains(attr),
            "登录页不得含内联事件属性 {}: {}",
            attr,
            html
        );
    }
    // 主题预涂脚本允许（1 处内联），其余内联 script 必须为 0
    let inline_scripts = html.matches("<script>").count();
    assert_eq!(
        inline_scripts, 1,
        "登录页应只剩 1 处预涂内联脚本，实际 {}",
        inline_scripts
    );

    // drive 页（认证）
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/drive")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let html = read_body_text(resp).await;
    for attr in [
        "onclick=",
        "onsubmit=",
        "onchange=",
        "oninput=",
        "onerror=",
        "onload=",
    ] {
        assert!(
            !html.contains(attr),
            "drive 页不得含内联事件属性 {}: {}",
            attr,
            html
        );
    }
    assert!(
        html.contains("/assets/app.js?v=2"),
        "drive 页应加载外部 app.js"
    );
}

// ====== v1.10.0 P0-4：符号链接逃逸回归（openat2 沙箱） ======

/// 沙箱外符号链接：读路径（media/编辑器/WebDAV）必须全部拒绝
#[tokio::test]
async fn test_symlink_escape_blocked_read_paths() {
    let (_pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;
    let auth = basic_auth_header(&username, "testpass123");

    // 种一个指向沙箱外（/etc/passwd）的符号链接
    std::os::unix::fs::symlink("/etc/passwd", format!("uploads/{}/evil.txt", username)).unwrap();

    // 1. 媒体代理
    let resp = app
        .clone()
        .oneshot(get_with_token(
            &format!("/api/media/{}", "evil.txt"),
            &token,
        ))
        .await
        .unwrap();
    assert!(
        resp.status().is_client_error(),
        "media 越界链接必须 4xx: {}",
        resp.status()
    );

    // 2. 在线编辑器读取
    let resp = app
        .clone()
        .oneshot(get_with_token("/api/edit/get?path=evil.txt", &token))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "编辑器越界链接必须 404"
    );

    // 3. WebDAV GET
    let resp = dav_send(&app, "GET", "/dav/evil.txt", Some(&auth), vec![], &[]).await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "WebDAV 越界链接必须 404"
    );

    // 4. PROPFIND 不得列出（越界条目视为不存在）
    let resp = dav_send(&app, "PROPFIND", "/dav/", Some(&auth), vec![], &[]).await;
    let xml = read_body_text(resp).await;
    assert!(
        !xml.contains("evil.txt"),
        "PROPFIND 不得暴露越界符号链接条目"
    );
}

/// 写路径：经符号链接目录的创建/重命名/移动必须拒绝
#[tokio::test]
async fn test_symlink_escape_blocked_write_paths() {
    let (_pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;
    let auth = basic_auth_header(&username, "testpass123");

    // 真实目录 + 指向沙箱外的符号链接目录
    std::fs::create_dir_all(format!("uploads/{}/sub", username)).unwrap();
    std::os::unix::fs::symlink("/tmp", format!("uploads/{}/evil_dir", username)).unwrap();

    // 1. 在符号链接目录下建文件夹 → 拒绝
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/create_folder",
            r#"{"name":"newdir","current_path":"evil_dir"}"#,
            &token,
        ))
        .await
        .unwrap();
    assert!(
        resp.status().is_client_error(),
        "经越界符号链接建目录必须拒绝: {}",
        resp.status()
    );

    // 2. 重命名进符号链接目录 → 拒绝
    std::fs::write(format!("uploads/{}/victim.txt", username), b"v").unwrap();
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/rename",
            r#"{"name":"victim.txt","new_name":"moved.txt","current_path":"evil_dir"}"#,
            &token,
        ))
        .await
        .unwrap();
    assert!(
        resp.status().is_client_error() || resp.status() == StatusCode::NOT_FOUND,
        "经越界符号链接重命名必须拒绝: {}",
        resp.status()
    );

    // 3. 移动目标为符号链接目录 → 拒绝
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/move",
            r#"{"name":"victim.txt","target_dir":"evil_dir","current_path":""}"#,
            &token,
        ))
        .await
        .unwrap();
    assert!(
        resp.status().is_client_error() || resp.status() == StatusCode::NOT_FOUND,
        "移入越界符号链接目录必须拒绝: {}",
        resp.status()
    );

    // 4. WebDAV PUT 到符号链接目录下 → 拒绝（非 2xx）
    let resp = dav_send(
        &app,
        "PUT",
        "/dav/evil_dir/x.txt",
        Some(&auth),
        b"x".to_vec(),
        &[],
    )
    .await;
    assert!(
        !resp.status().is_success(),
        "WebDAV 越界目录写入必须拒绝: {}",
        resp.status()
    );
}

/// 删除/重命名符号链接本身：只移除链接，绝不触碰链接目标
#[tokio::test]
async fn test_symlink_delete_unlinks_only_link() {
    let (_pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;

    std::fs::write(format!("uploads/{}/real_target.txt", username), b"PRECIOUS").unwrap();
    std::os::unix::fs::symlink(
        format!("uploads/{}/real_target.txt", username),
        format!("uploads/{}/lnk.txt", username),
    )
    .unwrap();

    // 删除链接（经 API：delete_to_trash 沙箱 rename 只移走链接本身）
    let resp = app
        .clone()
        .oneshot(post_json_with_token(
            "/api/files/delete",
            r#"{"name":"lnk.txt","current_path":""}"#,
            &token,
        ))
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "删除符号链接应成功: {}",
        resp.status()
    );
    assert!(
        std::fs::read_to_string(format!("uploads/{}/real_target.txt", username)).unwrap()
            == "PRECIOUS",
        "链接目标不得被触碰"
    );
}

/// 沙箱内相对符号链接仍可读（正常用户数据不受影响）
#[tokio::test]
async fn test_symlink_internal_relative_still_readable() {
    let (_pool, app) = test_app().await;
    let (token, username) = register_and_login_with_username(&app).await;

    std::fs::write(format!("uploads/{}/inner.txt", username), b"ok").unwrap();
    std::os::unix::fs::symlink("inner.txt", format!("uploads/{}/alias.txt", username)).unwrap();

    let resp = app
        .clone()
        .oneshot(get_with_token(
            &format!("/api/media/{}", "alias.txt"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "沙箱内相对链接应可读");
}

// ====== v1.10.0 P1-2：X-Request-Id 贯穿 ======

/// 所有响应（含未认证 401）必须携带 X-Request-Id；入站 ID 被沿用（dsh 链路关联）
#[tokio::test]
async fn test_request_id_header_present_and_echoed() {
    let (_pool, app) = test_app().await;

    // 未认证请求（401 也应带 ID）
    let resp = app
        .clone()
        .oneshot(get_with_token("/api/files/list", "invalid-token"))
        .await
        .unwrap();
    let rid = resp
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .expect("401 响应必须携带 X-Request-Id")
        .to_string();
    assert!(!rid.is_empty());

    // 入站携带 ID → 响应沿用同一 ID
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/health")
                .header("x-request-id", "trace-abc-123")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "trace-abc-123",
        "入站 X-Request-Id 必须原样沿用"
    );
}
