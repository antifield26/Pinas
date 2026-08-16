// ====== P0 回归测试：写路径 `..` parent / 源名校验（code_review.md P0-1/P0-2） ======
// 隔离环境双用户：attacker 尝试用 "../victim" parent 操作 victim 的文件必须被拒绝，
// 磁盘文件不得被移动/删除；`..` 源名同样拒绝；正常同用户操作不受影响。
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use pi_nas::config::Config;
use pi_nas::db;
use pi_nas::router;
use sqlx::SqlitePool;
use tower::util::ServiceExt;

static CWD_INIT: std::sync::Once = std::sync::Once::new();
fn ensure_test_cwd() {
    CWD_INIT.call_once(|| {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        std::mem::forget(dir);
    });
}

async fn setup() -> (SqlitePool, axum::Router) {
    ensure_test_cwd();
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .connect_with(
            "sqlite::memory:"
                .parse::<sqlx::sqlite::SqliteConnectOptions>()
                .unwrap()
                .foreign_keys(true),
        )
        .await
        .unwrap();
    db::init_test_db(&pool).await.unwrap();
    let app = router::build_router(Config::default(), pool.clone());
    (pool, app)
}

async fn register_login(app: &axum::Router, username: &str) -> String {
    let body = format!(r#"{{"username":"{}","password":"password123"}}"#, username);
    let req = Request::builder()
        .method("POST")
        .uri("/api/register")
        .header("content-type", "application/json")
        .body(Body::from(body.clone()))
        .unwrap();
    let _ = app.clone().oneshot(req).await.unwrap();
    let req = Request::builder()
        .method("POST")
        .uri("/api/login")
        .header("content-type", "application/json")
        // P2-4：响应 token 仅回显给显式 Bearer 请求方
        .header("authorization", "Bearer test-client")
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    v["token"].as_str().unwrap().to_string()
}

async fn post_file_api(app: &axum::Router, uri: &str, body: &str, token: &str) -> StatusCode {
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", token))
        .body(Body::from(body.to_string()))
        .unwrap();
    app.clone().oneshot(req).await.unwrap().status()
}

/// 场景底料：victim 在 secret_dir 下有 doc.txt；attacker 已登录
/// 用户名带全局计数器：限速器为进程级共享，且 register/login 共用 user:{name} 键，
/// 复用同名会导致后续测试被 429 限速（register 3 次/3600s）
static TEST_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn unique_name(tag: &str) -> String {
    let n = TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}_{}", tag, n)
}

async fn two_users_with_victim_file(app: &axum::Router) -> (String, String) {
    // 返回 (victim 用户名, attacker token)——各测试只断言自己的 victim 文件完好，
    // 避免并发下扫描其他测试尚未创建完成的目录（时序竞态）
    let victim = unique_name("victim");
    let attacker = unique_name("attacker");
    let _victim_token = register_login(app, &victim).await;
    let attacker_token = register_login(app, &attacker).await;
    std::fs::create_dir_all(format!("uploads/{}/secret_dir", victim)).unwrap();
    std::fs::write(
        format!("uploads/{}/secret_dir/doc.txt", victim),
        b"VICTIM-DATA",
    )
    .unwrap();
    (victim, attacker_token)
}

async fn assert_victim_file_intact(victim: &str) {
    let p = format!("uploads/{}/secret_dir/doc.txt", victim);
    assert!(
        std::path::Path::new(&p).exists(),
        "victim 文件应完好: {}",
        p
    );
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "VICTIM-DATA",
        "victim 文件内容不得被篡改: {}",
        p
    );
}

#[tokio::test]
async fn p0_reject_cross_user_move_with_dotdot_parent() {
    let (_pool, app) = setup().await;
    let (victim, at) = two_users_with_victim_file(&app).await;
    let body = format!(
        r#"{{"current_path":"../{}/secret_dir","target_dir":"","name":"doc.txt"}}"#,
        victim
    );
    let status = post_file_api(&app, "/api/files/move", &body, &at).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "跨用户 move 必须 400");
    assert_victim_file_intact(&victim).await;
    assert!(!std::path::Path::new("uploads/attacker10/doc.txt").exists());
}

#[tokio::test]
async fn p0_reject_cross_user_rename_with_dotdot_parent() {
    let (_pool, app) = setup().await;
    let (victim, at) = two_users_with_victim_file(&app).await;
    let body = format!(
        r#"{{"current_path":"../{}/secret_dir","name":"doc.txt","new_name":"stolen.txt"}}"#,
        victim
    );
    let status = post_file_api(&app, "/api/files/rename", &body, &at).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "跨用户 rename 必须 400");
    assert_victim_file_intact(&victim).await;
    assert!(!std::path::Path::new(&format!("uploads/{}/secret_dir/stolen.txt", victim)).exists());
}

#[tokio::test]
async fn p0_reject_cross_user_delete_with_dotdot_parent() {
    let (_pool, app) = setup().await;
    let (victim, at) = two_users_with_victim_file(&app).await;
    let body = format!(
        r#"{{"current_path":"../{}/secret_dir","name":"doc.txt"}}"#,
        victim
    );
    let status = post_file_api(&app, "/api/files/delete", &body, &at).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "跨用户 delete 必须 400");
    assert_victim_file_intact(&victim).await;
    // 回收站不得出现 victim 的文件（无条目被登记）
    let trash_count = std::fs::read_dir("uploads/.trash")
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(trash_count, 0, "回收站不得新增跨用户条目");
}

#[tokio::test]
async fn p0_reject_cross_user_merge_with_dotdot_parent() {
    let (_pool, app) = setup().await;
    let (victim, at) = two_users_with_victim_file(&app).await;
    // 先让 attacker 上传一个分片（multipart + query），再对 "../victim" 合并
    let boundary = "----p0boundary";
    let body = format!(
        "--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"x.bin\"\r\nContent-Type: application/octet-stream\r\n\r\nsome-chunk-data\r\n--{b}--\r\n",
        b = boundary
    );
    let req = Request::builder()
        .method("POST")
        .uri("/api/files/upload_chunk?identifier=p0-merge-test-1&chunk_index=0&total_chunks=1")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={}", boundary),
        )
        .header("authorization", format!("Bearer {}", at))
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert!(resp.status().is_success(), "分片应上传成功");
    let body = format!(
        r#"{{"identifier":"p0-merge-test-1","file_name":"planted.txt","parent_path":"../{}/secret_dir","total_chunks":1}}"#,
        victim
    );
    let status = post_file_api(&app, "/api/files/merge", &body, &at).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "跨用户 merge 必须 400");
    assert!(!std::path::Path::new(&format!("uploads/{}/secret_dir/planted.txt", victim)).exists());
}

#[tokio::test]
async fn p0_reject_dotdot_source_name_move() {
    let (_pool, app) = setup().await;
    let (_victim, at) = two_users_with_victim_file(&app).await;
    // attacker 用源名 ".." 尝试移走自己的用户根目录（应被拒绝）
    let body = r#"{"current_path":"dirA","target_dir":"","name":".."}"#;
    let status = post_file_api(&app, "/api/files/move", body, &at).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "源名 '..' move 必须 400");
}

#[tokio::test]
async fn p0_reject_dotdot_source_name_rename() {
    let (_pool, app) = setup().await;
    let (_victim, at) = two_users_with_victim_file(&app).await;
    let body = r#"{"current_path":"","name":"..","new_name":"moved_tree"}"#;
    let status = post_file_api(&app, "/api/files/rename", body, &at).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "源名 '..' rename 必须 400");
}

#[tokio::test]
async fn p0_normal_same_user_operations_still_work() {
    let (_pool, app) = setup().await;
    let (victim, at) = two_users_with_victim_file(&app).await;
    // attacker 在自己的目录内正常操作不受影响
    let status = post_file_api(
        &app,
        "/api/files/create_folder",
        r#"{"current_path":"","name":"my_dir"}"#,
        &at,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let status = post_file_api(
        &app,
        "/api/files/rename",
        r#"{"current_path":"","name":"my_dir","new_name":"my_dir2"}"#,
        &at,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // 目录操作成功（验证攻击者自身的正常操作未受影响）：任一 attacker_* 目录含 my_dir2
    let attacker_dirs: Vec<String> = std::fs::read_dir("uploads")
        .map(|d| {
            d.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("attacker_"))
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        attacker_dirs
            .iter()
            .any(|d| std::path::Path::new(&format!("uploads/{}/my_dir2", d)).is_dir()),
        "attacker 正常目录操作应成功"
    );
    // victim 的文件仍完好
    assert_victim_file_intact(&victim).await;
}
