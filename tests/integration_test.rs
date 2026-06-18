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
    let response = app.clone()
        .oneshot(post_json("/api/register", r#"{"username":"alice","password":"secret123"}"#))
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 重复注册应返回 CONFLICT
    let response = app.clone()
        .oneshot(post_json("/api/register", r#"{"username":"alice","password":"secret123"}"#))
        .await.unwrap();
    assert!(response.status() == StatusCode::CONFLICT || response.status() == StatusCode::BAD_REQUEST);

    // 正确登录
    let response = app.clone()
        .oneshot(post_json("/api/login", r#"{"username":"alice","password":"secret123"}"#))
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), 1024).await.unwrap();
    let login_data: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let token = login_data["token"].as_str().unwrap().to_string();

    // 使用 token 访问受保护路由
    let response = app.clone()
        .oneshot(get_with_token("/api/files/list", &token))
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 错误密码登录应返回 401
    let response = app.clone()
        .oneshot(post_json("/api/login", r#"{"username":"alice","password":"wrongpass"}"#))
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // 登出
    let response = app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/logout")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 登出后 token 应失效
    let response = app
        .oneshot(get_with_token("/api/files/list", &token))
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// ====== 2. 文件 CRUD 测试 ======

#[tokio::test]
async fn test_file_create_list_delete_restore() {
    let (_pool, app) = test_app().await;

    // 注册并登录
    let _ = app.clone()
        .oneshot(post_json("/api/register", r#"{"username":"bob","password":"pass456"}"#))
        .await.unwrap();
    let response = app.clone()
        .oneshot(post_json("/api/login", r#"{"username":"bob","password":"pass456"}"#))
        .await.unwrap();
    let body = axum::body::to_bytes(response.into_body(), 1024).await.unwrap();
    let token: String = {
        let data: serde_json::Value = serde_json::from_slice(&body).unwrap();
        data["token"].as_str().unwrap().to_string()
    };

    // 创建文件夹
    let response = app.clone()
        .oneshot(post_json_with_token(
            "/api/files/create_folder",
            r#"{"name":"docs","current_path":null}"#,
            &token,
        ))
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 列出文件（验证文件夹存在）
    let response = app.clone()
        .oneshot(get_with_token("/api/files/list?path=", &token))
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
    let list: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let items = list["items"].as_array().unwrap();
    assert!(items.iter().any(|f| f["name"] == "docs" && f["is_dir"] == 1));

    // 重命名文件夹
    let response = app.clone()
        .oneshot(post_json_with_token(
            "/api/files/rename",
            r#"{"name":"docs","new_name":"documents","current_path":null}"#,
            &token,
        ))
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 验证重命名
    let response = app.clone()
        .oneshot(get_with_token("/api/files/list?path=", &token))
        .await.unwrap();
    let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
    let list: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let items = list["items"].as_array().unwrap();
    assert!(items.iter().any(|f| f["name"] == "documents"));
    assert!(!items.iter().any(|f| f["name"] == "docs"));

    // 删除（移至回收站）
    let response = app.clone()
        .oneshot(post_json_with_token(
            "/api/files/delete",
            r#"{"name":"documents","current_path":null}"#,
            &token,
        ))
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 回收站中应包含已删除项
    let response = app.clone()
        .oneshot(get_with_token("/api/trash/list", &token))
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
    let trash: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert!(!trash.is_empty());
    let trash_id = trash[0]["id"].as_i64().unwrap();

    // 从回收站还原
    let response = app.clone()
        .oneshot(post_json_with_token(
            "/api/trash/restore",
            &format!(r#"{{"id":{}}}"#, trash_id),
            &token,
        ))
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 验证文件已还原
    let response = app
        .oneshot(get_with_token("/api/files/list?path=", &token))
        .await.unwrap();
    let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
    let list: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let items = list["items"].as_array().unwrap();
    assert!(items.iter().any(|f| f["name"] == "documents"));
}

// ====== 3. 分享流程测试 ======

#[tokio::test]
async fn test_share_create_access_delete() {
    let (_pool, app) = test_app().await;

    // 注册并登录
    let _ = app.clone()
        .oneshot(post_json("/api/register", r#"{"username":"carol","password":"share123"}"#))
        .await.unwrap();
    let response = app.clone()
        .oneshot(post_json("/api/login", r#"{"username":"carol","password":"share123"}"#))
        .await.unwrap();
    let body = axum::body::to_bytes(response.into_body(), 1024).await.unwrap();
    let token: String = {
        let data: serde_json::Value = serde_json::from_slice(&body).unwrap();
        data["token"].as_str().unwrap().to_string()
    };

    // 创建分享（无密码，1小时过期）
    let response = app.clone()
        .oneshot(post_json_with_token(
            "/api/share/create",
            r#"{"file_path":"/test.txt","is_dir":false,"expire_hours":1}"#,
            &token,
        ))
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024).await.unwrap();
    let share_data: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let share_code = share_data["code"].as_str().unwrap().to_string();
    // 验证为完整 UUID（36 字符，含连字符）
    assert_eq!(share_code.len(), 36);
    assert!(share_code.contains('-'));

    // 列出分享
    let response = app.clone()
        .oneshot(get_with_token("/api/share/list", &token))
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
    let shares: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert!(shares.iter().any(|s| s["code"] == share_code));

    // 匿名访问分享（验证链接有效）
    let response = app.clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/share/access/{}", share_code))
                .body(Body::empty())
                .unwrap(),
        )
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 删除分享
    let response = app.clone()
        .oneshot(post_json_with_token(
            "/api/share/delete",
            &format!(r#"{{"code":"{}"}}"#, share_code),
            &token,
        ))
        .await.unwrap();
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
        .await.unwrap();
    assert!(
        response.status() == StatusCode::GONE || response.status() == StatusCode::NOT_FOUND
    );
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
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1024).await.unwrap();
    let health: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(health["status"], "healthy");
}
