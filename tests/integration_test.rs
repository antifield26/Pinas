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

/// 创建测试应用（内存 SQLite，仅建表不含默认用户）
async fn test_app() -> (SqlitePool, axum::Router) {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    db::init_test_db(&pool).await.unwrap();
    let config = Config::default();
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
    assert!(
        !cookie2.contains("; Secure"),
        "直连 http 场景不应带 Secure: {}",
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

    // 匿名访问分享（验证链接有效）
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
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let username = format!("testuser_{}", timestamp);

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
