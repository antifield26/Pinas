use axum::{
    extract::{Extension, Query},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Row};

use crate::core::UserSession;
use crate::db::queries::update_child_paths as update_child_parent_paths;
use crate::error::{AppError, AppResult};
use crate::handlers::utils::{
    bytes_to_mb_string, log_audit, safe_join_sandbox, update_user_used_mb, user_dir_path,
};
use crate::templates::AppTemplate;
use askama::Template;

// ====== DTOs ======

#[derive(Deserialize)]
pub struct CreateFolderRequest {
    pub name: String,
    pub current_path: Option<String>,
}

#[derive(Deserialize)]
pub struct RenameRequest {
    pub name: String,
    pub new_name: String,
    pub current_path: Option<String>,
}

#[derive(Deserialize)]
pub struct MoveRequest {
    pub name: String,
    pub target_dir: String,
    pub current_path: Option<String>,
}

#[derive(Deserialize)]
pub struct MoveBatchRequest {
    pub names: Vec<String>,
    pub current_path: Option<String>,
    pub target_path: String,
}

#[derive(Deserialize, Debug)]
pub struct ListFilesQuery {
    pub path: Option<String>,
    pub search: Option<String>,
    pub sort_by: Option<String>,
    pub page: Option<i64>,
    pub page_size: Option<i64>,
}

#[derive(Serialize, FromRow)]
pub struct FileItem {
    pub id: i64,
    pub name: String,
    pub parent_path: String,
    pub is_dir: i64,
    pub size_mb: Option<f64>,
    pub created_at: String,
}

#[derive(Deserialize)]
pub struct DeleteRequest {
    pub name: String,
    pub current_path: Option<String>,
}

#[derive(Deserialize)]
pub struct BatchDeleteRequest {
    pub names: Vec<String>,
    pub current_path: Option<String>,
}

// ====== 公共 Helper 函数 ======

/// 构建用户文件的物理路径: `uploads/{username}/{parent}/{name}`
fn user_file_path(username: &str, parent: &str, name: &str) -> String {
    if parent.is_empty() {
        format!("{}/{}", username, name)
    } else {
        format!("{}/{}/{}", username, parent, name)
    }
}

/// 构建完整的目录路径: `{parent}/{name}`，parent 为空时返回 name
fn logical_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{}/{}", parent, name)
    }
}

/// 格式化文件大小用于显示（统一入口）
fn fmt_size(mb: f64, is_dir: bool) -> String {
    if is_dir {
        return String::from("--");
    }
    if mb <= 0.0 {
        String::from("0 KB")
    } else if mb < 0.001 {
        format!("{:.2} KB", mb * 1024.0)
    } else if mb < 1.0 {
        format!("{:.1} KB", mb * 1024.0)
    } else {
        format!("{:.1} MB", mb)
    }
}

/// 批量磁盘存在性校验 + 孤儿 DB 记录清理（列表热路径）：
/// 逐行 stat+DELETE 会造成 N 次顺序 await 与 N 个 WAL 写事务（每行还触发 FTS 触发器），
/// 改为并发 try_exists + 单条批量 DELETE。返回磁盘上实际存在的 (name, parent_path) 集合。
async fn reconcile_files_on_disk(
    pool: &sqlx::SqlitePool,
    username: &str,
    rows: &[(String, String, bool)], // (name, parent_path, is_dir)
) -> std::collections::HashSet<(String, String)> {
    use std::collections::HashSet;
    let base = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let checks = rows.iter().map(|(name, parent_path, is_dir)| async move {
        let rel = user_file_path(username, parent_path, name);
        let exists = match safe_join_sandbox(base, &rel) {
            Ok(full) => match tokio::fs::metadata(&full).await {
                Ok(meta) => {
                    if *is_dir {
                        meta.is_dir()
                    } else {
                        meta.is_file()
                    }
                }
                Err(_) => false,
            },
            // 路径非法(穿越)视为不存在，交由调用方清理 DB 记录
            Err(_) => false,
        };
        ((name.clone(), parent_path.clone()), exists)
    });
    let results = futures_util::future::join_all(checks).await;

    let mut present: HashSet<(String, String)> = HashSet::with_capacity(results.len());
    let mut missing: Vec<(String, String)> = Vec::new();
    for ((name, parent), exists) in results {
        if exists {
            present.insert((name, parent));
        } else {
            missing.push((name, parent));
        }
    }
    if !missing.is_empty() {
        tracing::warn!("[文件同步] 批量清理 {} 条磁盘缺失记录", missing.len());
        let mut qb = sqlx::QueryBuilder::new("DELETE FROM files WHERE username = ");
        qb.push_bind(username);
        qb.push(" AND (name, parent_path) IN (");
        let mut first = true;
        for (name, parent) in &missing {
            if !first {
                qb.push(",");
            }
            first = false;
            qb.push("(")
                .push_bind(name)
                .push(",")
                .push_bind(parent)
                .push(")");
        }
        qb.push(")");
        let _ = qb.build().execute(pool).await;
    }
    present
}

/// 标准化显示路径
fn normalize_display_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return "/".to_string();
    }
    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    format!("/{}", segments.join("/"))
}

/// MIME 类型规范化（浏览器兼容）
fn normalize_preview_mime(file_name: &str, default_mime: mime_guess::Mime) -> mime_guess::Mime {
    let ext = std::path::Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "m4v" | "mp4v" => "video/mp4".parse().unwrap_or(default_mime),
        "m4a" => "audio/mp4".parse().unwrap_or(default_mime),
        _ => default_mime,
    }
}

/// 文件系统操作回滚辅助
fn rollback_rename(src: &std::path::Path, dst: &std::path::Path) {
    let _ = std::fs::rename(dst, src);
}

/// 批量移动回滚：逆序把已移动的条目移回原位（同步 fs，回滚路径可接受阻塞）
fn rollback_batch_moves(moved: &[(String, std::path::PathBuf, std::path::PathBuf)]) {
    for (_, s, d) in moved.iter().rev() {
        let _ = std::fs::rename(d, s);
    }
}

// ====== 文件列表查询 ======

/// 查询文件列表并过滤磁盘缺失项
async fn query_files(
    pool: &sqlx::SqlitePool,
    username: &str,
    path: &str,
    search: Option<&str>,
    sort_by: Option<&str>,
) -> Vec<FileRowData> {
    let parent_path = path.trim_start_matches('/');
    let search = search.map(str::trim).filter(|s| !s.is_empty());
    let mut qb = sqlx::QueryBuilder::new(
        "SELECT name, size_mb, is_dir, parent_path FROM files WHERE username = ",
    );
    qb.push_bind(username);
    if let Some(s) = &search {
        let s_raw = (*s).to_string();
        let s = crate::db::queries::escape_like(s);
        if parent_path.is_empty() {
            // 全局搜索（path 为空）：≥3 字符走 FTS5 trigram（子串匹配含中文），≤2 字符降级 LIKE 兜底
            if s_raw.chars().count() >= 3 {
                qb.push(" AND id IN (SELECT rowid FROM files_fts WHERE files_fts MATCH ");
                qb.push_bind(format!("\"{}\"", s_raw.replace('"', "\"\"")));
                qb.push(")");
            } else {
                qb.push(" AND (name LIKE ")
                    .push_bind(format!("%{}%", s))
                    .push(" ESCAPE '\\'");
                qb.push(" OR parent_path LIKE ")
                    .push_bind(format!("%{}%", s))
                    .push(" ESCAPE '\\'");
                qb.push(")");
            }
        } else {
            qb.push(" AND parent_path = ").push_bind(parent_path);
            qb.push(" AND name LIKE ")
                .push_bind(format!("%{}%", s))
                .push(" ESCAPE '\\'");
        }
    } else {
        qb.push(" AND parent_path = ").push_bind(parent_path);
    }
    let order = match sort_by.unwrap_or("") {
        "name_desc" => " ORDER BY is_dir DESC, name DESC",
        "size_asc" => " ORDER BY is_dir DESC, size_mb ASC",
        "size_desc" => " ORDER BY is_dir DESC, size_mb DESC",
        "time_desc" => " ORDER BY is_dir DESC, created_at DESC",
        "time_asc" => " ORDER BY is_dir DESC, created_at ASC",
        _ => " ORDER BY is_dir DESC, name ASC",
    };
    qb.push(order);
    // 防御上限：HTMX 片段列表无分页 UI（JSON API 路径有分页），
    // 超量目录（10 万级文件）一次性全量渲染会打爆响应与前端
    qb.push(" LIMIT 1000");

    #[derive(sqlx::FromRow)]
    struct FileRowRaw {
        name: String,
        size_mb: Option<f64>,
        is_dir: i64,
        parent_path: String,
    }
    match qb.build_query_as::<FileRowRaw>().fetch_all(pool).await {
        Ok(rows) => {
            let batch: Vec<(String, String, bool)> = rows
                .iter()
                .map(|r| (r.name.clone(), r.parent_path.clone(), r.is_dir != 0))
                .collect();
            let present = reconcile_files_on_disk(pool, username, &batch).await;
            rows.into_iter()
                .filter(|r| present.contains(&(r.name.clone(), r.parent_path.clone())))
                .map(|r| FileRowData {
                    name: r.name,
                    is_dir: r.is_dir != 0,
                    size_display: fmt_size(r.size_mb.unwrap_or(0.0), r.is_dir != 0),
                    parent_path: r.parent_path,
                })
                .collect()
        }
        Err(e) => {
            tracing::error!("[Drive] 文件列表查询失败: {}", e);
            Vec::new()
        }
    }
}

/// HTMX 错误恢复：重新查询文件列表并返回模板，同时触发配额刷新。
/// 返回具体类型 Response（两个 fallback 变体供同一 handler 分支共用，opaque impl Trait 会冲突）
async fn fallback_file_list(
    pool: sqlx::SqlitePool,
    username: String,
    path: String,
) -> axum::response::Response {
    let files = query_files(&pool, &username, &path, None, None).await;
    let mut resp = AppTemplate(FileTableFragment {
        files,
        current_path: path,
        has_global_search: false,
        search: String::new(),
    })
    .into_response();
    resp.headers_mut().insert(
        "HX-Trigger",
        axum::http::HeaderValue::from_static("quotaRefresh"),
    );
    resp
}

/// HTMX 错误恢复 + 错误提示：列表照常刷新，同时经 HX-Trigger(JSON) 派发 toastError 事件
/// （base.html 监听并弹错误 Toast）——历史实现错误被静默吞掉，用户看到"操作成功"的假象
async fn fallback_file_list_with_error(
    pool: sqlx::SqlitePool,
    username: String,
    path: String,
    msg: &str,
) -> axum::response::Response {
    let files = query_files(&pool, &username, &path, None, None).await;
    let mut resp = AppTemplate(FileTableFragment {
        files,
        current_path: path,
        has_global_search: false,
        search: String::new(),
    })
    .into_response();
    let trigger = format!(
        "{{\"toastError\": {}}}",
        serde_json::to_string(msg).unwrap_or_else(|_| "\"操作失败\"".to_string())
    );
    resp.headers_mut().insert(
        "HX-Trigger",
        axum::http::HeaderValue::from_str(&trigger)
            .unwrap_or(axum::http::HeaderValue::from_static("quotaRefresh")),
    );
    resp
}

// ====== 1. JSON API: 文件列表 ======

fn bind_list_where(
    qb: &mut sqlx::QueryBuilder<sqlx::Sqlite>,
    username: &str,
    current_path: &str,
    has_search: bool,
    search_raw: &str,
    search_pattern: &str,
    like_pattern: &Option<String>,
) {
    qb.push(" WHERE username = ").push_bind(username);
    if has_search {
        if current_path.is_empty() {
            // 全局搜索：≥3 字符走 FTS5 trigram（子串匹配含中文），≤2 字符降级 LIKE 兜底（trigram 限制）
            if search_raw.chars().count() >= 3 {
                qb.push(" AND id IN (SELECT rowid FROM files_fts WHERE files_fts MATCH ");
                qb.push_bind(format!("\"{}\"", search_raw.replace('"', "\"\"")));
                qb.push(")");
            } else {
                qb.push(" AND (name LIKE ")
                    .push_bind(search_pattern)
                    .push(" ESCAPE '\\'");
                qb.push(" OR parent_path LIKE ")
                    .push_bind(search_pattern)
                    .push(" ESCAPE '\\'");
                qb.push(")");
            }
        } else {
            qb.push(" AND (parent_path = ").push_bind(current_path);
            qb.push(" OR parent_path LIKE ")
                .push_bind(like_pattern.as_deref().unwrap_or(""))
                .push(" ESCAPE '\\'");
            qb.push(") AND name LIKE ")
                .push_bind(search_pattern)
                .push(" ESCAPE '\\'");
        }
    } else {
        qb.push(" AND parent_path = ").push_bind(current_path);
    }
}

#[tracing::instrument(skip_all)]
pub async fn list_files(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Query(query): Query<ListFilesQuery>,
) -> impl IntoResponse {
    let current_path = user_dir_path(query.path);
    let has_search = query
        .search
        .as_ref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let search_raw = query.search.as_deref().unwrap_or("");
    // LIKE 通配符转义：用户输入中的 %/_ 不得意外匹配其他行（制造昂贵的全表扫描）
    let search_pattern = if has_search {
        format!("%{}%", crate::db::queries::escape_like(search_raw))
    } else {
        String::new()
    };
    let like_pattern = if has_search && !current_path.is_empty() {
        Some(format!("{}/%", current_path))
    } else {
        None
    };
    let page = query.page.unwrap_or(1).max(1);
    // clamp 下界：负 LIMIT 在 SQLite 中意为"无限制"，0 会除零/返回畸形分页
    let page_size = query
        .page_size
        .unwrap_or(crate::constants::DEFAULT_PAGE_SIZE)
        .clamp(1, crate::constants::MAX_PAGE_SIZE);
    let offset = (page - 1).saturating_mul(page_size);
    let order_sql = match query.sort_by.as_deref() {
        Some("name_desc") => " ORDER BY is_dir DESC, name DESC",
        Some("size_asc") => " ORDER BY is_dir DESC, size_mb ASC",
        Some("size_desc") => " ORDER BY is_dir DESC, size_mb DESC",
        Some("time_desc") => " ORDER BY is_dir DESC, created_at DESC",
        Some("time_asc") => " ORDER BY is_dir DESC, created_at ASC",
        _ => " ORDER BY is_dir DESC, name ASC",
    };

    let total: i64 = {
        let mut cq = sqlx::QueryBuilder::new("SELECT COUNT(*) FROM files");
        bind_list_where(
            &mut cq,
            &session.username,
            &current_path,
            has_search,
            search_raw,
            &search_pattern,
            &like_pattern,
        );
        cq.build_query_scalar().fetch_one(&pool).await.unwrap_or(0)
    };

    let mut dq = sqlx::QueryBuilder::new(
        "SELECT id, name, parent_path, is_dir, size_mb, created_at FROM files",
    );
    bind_list_where(
        &mut dq,
        &session.username,
        &current_path,
        has_search,
        search_raw,
        &search_pattern,
        &like_pattern,
    );
    dq.push(order_sql)
        .push(" LIMIT ")
        .push_bind(page_size)
        .push(" OFFSET ")
        .push_bind(offset);

    match dq.build_query_as::<FileItem>().fetch_all(&pool).await {
        Ok(files) => {
            // 批量磁盘校验（并发 try_exists + 单条批量 DELETE，替代逐行 stat+DELETE 的 N+1 写放大）
            let batch: Vec<(String, String, bool)> = files
                .iter()
                .map(|f| (f.name.clone(), f.parent_path.clone(), f.is_dir != 0))
                .collect();
            let present = reconcile_files_on_disk(&pool, &session.username, &batch).await;
            let valid: Vec<FileItem> = files
                .into_iter()
                .filter(|f| present.contains(&(f.name.clone(), f.parent_path.clone())))
                .collect();
            Json(serde_json::json!({"items": valid, "page": page, "page_size": page_size, "total": total, "total_pages": ((total as f64)/(page_size as f64)).ceil() as i64})).into_response()
        }
        Err(e) => {
            tracing::error!("[Files] 列表查询失败: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "读取文件列表失败").into_response()
        }
    }
}

// ====== 2. 创建文件夹 ======

fn create_folder_common(
    username: &str,
    parent: &str,
    name: &str,
    base: &std::path::Path,
) -> AppResult<std::path::PathBuf> {
    crate::handlers::utils::validate_name(name)?;
    let sub = if parent.is_empty() {
        name.to_string()
    } else {
        format!("{}/{}", parent, name)
    };
    safe_join_sandbox(base, &format!("{}/{}", username, sub))
}

#[tracing::instrument(skip_all)]
pub async fn create_folder(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<CreateFolderRequest>,
) -> AppResult<(StatusCode, &'static str)> {
    let name = payload.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::bad_request("名称不能为空"));
    }
    let parent = user_dir_path(payload.current_path);
    let target = create_folder_common(
        &session.username,
        &parent,
        &name,
        std::path::Path::new(crate::constants::UPLOADS_DIR),
    )?;
    // M11：INSERT 先行——UNIQUE 约束是唯一真相源。历史顺序「预检 → 建目录 → INSERT」
    // 存在并发竞态：两个同名请求都过预检，后到者 INSERT 冲突后 remove_dir 会删掉
    // 先到者刚建好的目录、留下幽灵行。INSERT 失败直接 conflict，绝不触碰目录。
    match sqlx::query("INSERT INTO files (username, name, parent_path, is_dir) VALUES (?, ?, ?, 1)")
        .bind(&session.username)
        .bind(&name)
        .bind(&parent)
        .execute(&pool)
        .await
    {
        Ok(_) => {}
        Err(e)
            if e.as_database_error()
                .is_some_and(|d| d.is_unique_violation()) =>
        {
            return Err(AppError::conflict("同名文件夹已存在"));
        }
        Err(e) => return Err(e.into()),
    }
    if let Err(e) = tokio::fs::create_dir_all(&target).await {
        // 物理创建失败：回收刚登记的 DB 行，不留幽灵行（DB→磁盘方向，reconcile 可修复）
        let _ = sqlx::query(
            "DELETE FROM files WHERE username = ? AND name = ? AND parent_path = ? AND is_dir = 1",
        )
        .bind(&session.username)
        .bind(&name)
        .bind(&parent)
        .execute(&pool)
        .await;
        tracing::error!("[Files] 创建目录失败: {}", e);
        return Err(AppError::internal("操作失败"));
    }
    let _ = log_audit(
        &pool,
        &session.username,
        "create_folder",
        Some(&logical_path(&parent, &name)),
        None,
        None,
        None,
    )
    .await;
    Ok((StatusCode::OK, "文件夹创建成功"))
}

/// 幂等补插逻辑目录行：当写入目标位于尚未登记的目录(如文件夹上传/WebDAV PUT 到新子目录)时，
/// 物理目录由调用方 create_dir_all 创建，此处逐级补插 files 目录行，保证"文件即真相"同步基准一致。
/// parent_path 为规范化逻辑路径(不含用户名,空串=根)；每级 INSERT OR IGNORE,重复调用无副作用。
pub(crate) async fn ensure_dir_rows(
    pool: &sqlx::SqlitePool,
    username: &str,
    parent_path: &str,
) -> Result<(), AppError> {
    let mut prefix = String::new();
    for seg in parent_path.split('/').filter(|s| !s.is_empty()) {
        sqlx::query(
            "INSERT OR IGNORE INTO files (username, name, parent_path, is_dir) VALUES (?, ?, ?, 1)",
        )
        .bind(username)
        .bind(seg)
        .bind(&prefix)
        .execute(pool)
        .await?;
        if prefix.is_empty() {
            prefix = seg.to_string();
        } else {
            prefix = format!("{}/{}", prefix, seg);
        }
    }
    Ok(())
}

// ====== 3. 重命名（共享核心逻辑） ======

/// 核心重命名逻辑：意图日志（M1）先行 → 物理 rename → DB 事务 → 删除日志。
/// 崩溃于任一步骤时，启动重放补齐/回滚，消除「FS 与 DB 两步非原子」的孤儿窗口
pub(crate) async fn rename_core(
    pool: &sqlx::SqlitePool,
    username: &str,
    parent: &str,
    old_name: &str,
    new_name: &str,
) -> AppResult<()> {
    if old_name == new_name {
        return Ok(());
    }
    crate::handlers::utils::validate_name(new_name)?;
    let base = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let old_p = safe_join_sandbox(base, &user_file_path(username, parent, old_name))?;
    let new_p = safe_join_sandbox(base, &user_file_path(username, parent, new_name))?;

    // 目标冲突预检：fs rename 会原子覆盖已存在目标，而随后的 DB UNIQUE 冲突回滚
    // 只能把新文件恢复回旧名，被覆盖的旧目标内容已永久丢失。必须先拒绝。
    let target_row_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM files WHERE username = ? AND name = ? AND parent_path = ?)",
    )
    .bind(username)
    .bind(new_name)
    .bind(parent)
    .fetch_one(pool)
    .await
    .unwrap_or(true); // DB 异常时保守拒绝，绝不冒险覆盖
    if target_row_exists || tokio::fs::try_exists(&new_p).await.unwrap_or(true) {
        return Err(AppError::conflict("目标名称已存在，请先移动或删除"));
    }

    let old_logical = logical_path(parent, old_name);
    let new_logical = logical_path(parent, new_name);
    let jid =
        crate::handlers::journal::insert(pool, username, "rename", &old_logical, &new_logical)
            .await?;

    if let Err(e) = tokio::fs::rename(&old_p, &new_p).await {
        crate::handlers::journal::remove(pool, jid).await;
        return Err(AppError::internal_log("文件系统重命名", e));
    }

    if let Err(e) = crate::handlers::journal::apply_db_rename_move(
        pool,
        username,
        "rename",
        &old_logical,
        &new_logical,
    )
    .await
    {
        rollback_rename(&old_p, &new_p);
        crate::handlers::journal::remove(pool, jid).await;
        return Err(e);
    }

    crate::handlers::journal::remove(pool, jid).await;
    Ok(())
}

#[tracing::instrument(skip_all)]
pub async fn rename_item(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<RenameRequest>,
) -> impl IntoResponse {
    if crate::handlers::utils::validate_name(&payload.new_name).is_err() {
        return (StatusCode::BAD_REQUEST, "名称包含非法字符").into_response();
    }
    let parent = user_dir_path(payload.current_path);
    let old_path = logical_path(&parent, &payload.name);
    match rename_core(
        &pool,
        &session.username,
        &parent,
        &payload.name,
        &payload.new_name,
    )
    .await
    {
        Ok(()) => {
            let _ = log_audit(
                &pool,
                &session.username,
                "rename",
                Some(&old_path),
                Some(&format!("-> {}", payload.new_name)),
                None,
                None,
            )
            .await;
            (StatusCode::OK, "重命名成功").into_response()
        }
        Err(AppError::Conflict(msg)) => (StatusCode::CONFLICT, msg).into_response(),
        Err(e) => {
            tracing::error!("[Files] 重命名失败: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "操作失败").into_response()
        }
    }
}

// ====== 4. 移动（共享核心逻辑） ======

pub(crate) async fn move_core(
    pool: &sqlx::SqlitePool,
    username: &str,
    src_parent: &str,
    dst_parent: &str,
    name: &str,
) -> AppResult<()> {
    if src_parent == dst_parent {
        return Ok(());
    }
    let base = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let src_p = safe_join_sandbox(base, &user_file_path(username, src_parent, name))?;
    let dst_p = safe_join_sandbox(base, &user_file_path(username, dst_parent, name))?;

    // 目标冲突预检：同 rename_core，防止 fs rename 覆盖同名目标造成永久数据丢失
    let target_row_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM files WHERE username = ? AND name = ? AND parent_path = ?)",
    )
    .bind(username)
    .bind(name)
    .bind(dst_parent)
    .fetch_one(pool)
    .await
    .unwrap_or(true);
    if target_row_exists || tokio::fs::try_exists(&dst_p).await.unwrap_or(true) {
        return Err(AppError::conflict("目标目录存在同名文件，请先移动或删除"));
    }

    // 意图日志先行（M1）：崩溃后启动重放补齐
    let src_logical = logical_path(src_parent, name);
    let dst_logical = logical_path(dst_parent, name);
    let jid = crate::handlers::journal::insert(pool, username, "move", &src_logical, &dst_logical)
        .await?;

    if let Err(e) = tokio::fs::rename(&src_p, &dst_p).await {
        crate::handlers::journal::remove(pool, jid).await;
        return Err(AppError::internal_log("文件系统移动", e));
    }

    if let Err(e) = crate::handlers::journal::apply_db_rename_move(
        pool,
        username,
        "move",
        &src_logical,
        &dst_logical,
    )
    .await
    {
        rollback_rename(&src_p, &dst_p);
        crate::handlers::journal::remove(pool, jid).await;
        return Err(e);
    }

    crate::handlers::journal::remove(pool, jid).await;
    Ok(())
}

#[tracing::instrument(skip_all)]
pub async fn move_item(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<MoveRequest>,
) -> impl IntoResponse {
    let src = user_dir_path(payload.current_path);
    let dst = user_dir_path(Some(payload.target_dir));
    let old_path = logical_path(&src, &payload.name);
    match move_core(&pool, &session.username, &src, &dst, &payload.name).await {
        Ok(()) => {
            let _ = log_audit(
                &pool,
                &session.username,
                "move",
                Some(&old_path),
                Some(&format!("-> {}", dst)),
                None,
                None,
            )
            .await;
            (StatusCode::OK, "迁移路径成功").into_response()
        }
        Err(AppError::Conflict(msg)) => (StatusCode::CONFLICT, msg).into_response(),
        Err(e) => {
            tracing::error!("[Files] 移动失败: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "操作失败").into_response()
        }
    }
}

// ====== 5. 批量移动 ======

#[tracing::instrument(skip_all)]
pub async fn move_batch(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<MoveBatchRequest>,
) -> impl IntoResponse {
    let src = user_dir_path(payload.current_path);
    let dst = user_dir_path(Some(payload.target_path));
    let base = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let mut moved: Vec<(String, std::path::PathBuf, std::path::PathBuf)> = Vec::new();
    // 意图日志（M1）：每个条目在物理移动前落一条记录，崩溃后逐条重放
    let mut jids: Vec<i64> = Vec::new();

    for name in &payload.names {
        let sp = match safe_join_sandbox(base, &user_file_path(&session.username, &src, name)) {
            Ok(p) => p,
            Err(_) => {
                rollback_batch_moves(&moved);
                crate::handlers::journal::remove_many(&pool, &jids).await;
                return (StatusCode::BAD_REQUEST, "包含非法路径").into_response();
            }
        };
        let dp = match safe_join_sandbox(base, &user_file_path(&session.username, &dst, name)) {
            Ok(p) => p,
            Err(_) => {
                rollback_batch_moves(&moved);
                crate::handlers::journal::remove_many(&pool, &jids).await;
                return (StatusCode::BAD_REQUEST, "包含非法路径").into_response();
            }
        };
        // 目标冲突预检：fs rename 覆盖已存在目标即永久销毁其内容（同 rename_core/move_core）
        if tokio::fs::try_exists(&dp).await.unwrap_or(true) {
            rollback_batch_moves(&moved);
            crate::handlers::journal::remove_many(&pool, &jids).await;
            return (StatusCode::CONFLICT, format!("目标存在同名文件: {}", name)).into_response();
        }
        let jid = match crate::handlers::journal::insert(
            &pool,
            &session.username,
            "move",
            &logical_path(&src, name),
            &logical_path(&dst, name),
        )
        .await
        {
            Ok(id) => id,
            Err(_) => {
                rollback_batch_moves(&moved);
                crate::handlers::journal::remove_many(&pool, &jids).await;
                return (StatusCode::INTERNAL_SERVER_ERROR, "操作失败，请稍后重试").into_response();
            }
        };
        jids.push(jid);
        if let Err(e) = tokio::fs::rename(&sp, &dp).await {
            tracing::error!("[Files] 批量移动 '{}' 失败: {}", name, e);
            rollback_batch_moves(&moved);
            crate::handlers::journal::remove_many(&pool, &jids).await;
            return (StatusCode::INTERNAL_SERVER_ERROR, "移动失败，操作已回滚").into_response();
        }
        moved.push((name.clone(), sp, dp));
    }

    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            rollback_batch_moves(&moved);
            crate::handlers::journal::remove_many(&pool, &jids).await;
            tracing::error!("[Files] 批量移动事务失败: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "操作失败，请稍后重试").into_response();
        }
    };

    for (name, _, _) in &moved {
        if let Err(e) = sqlx::query(
            "UPDATE files SET parent_path = ? WHERE username = ? AND name = ? AND parent_path = ?",
        )
        .bind(&dst)
        .bind(&session.username)
        .bind(name)
        .bind(&src)
        .execute(&mut *tx)
        .await
        {
            drop(tx);
            rollback_batch_moves(&moved);
            crate::handlers::journal::remove_many(&pool, &jids).await;
            tracing::error!("[Files] 批量移动数据库更新失败: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "操作失败，请稍后重试").into_response();
        }
        if let Err(e) = update_child_parent_paths(
            &mut tx,
            &session.username,
            &logical_path(&src, name),
            &logical_path(&dst, name),
        )
        .await
        {
            drop(tx);
            rollback_batch_moves(&moved);
            crate::handlers::journal::remove_many(&pool, &jids).await;
            tracing::error!("[Files] 批量移动子路径更新失败: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "操作失败，请稍后重试").into_response();
        }
    }
    if let Err(e) = tx.commit().await {
        rollback_batch_moves(&moved);
        crate::handlers::journal::remove_many(&pool, &jids).await;
        tracing::error!("[Files] 批量移动提交失败: {}", e);
        return (StatusCode::INTERNAL_SERVER_ERROR, "操作失败，请稍后重试").into_response();
    }

    // 全部成功：清除意图日志
    crate::handlers::journal::remove_many(&pool, &jids).await;

    let _ = log_audit(
        &pool,
        &session.username,
        "move_batch",
        Some(&format!("[{}]", payload.names.join(","))),
        Some(&format!("{} -> {}", src, dst)),
        None,
        None,
    )
    .await;
    (StatusCode::OK, "批量移动成功").into_response()
}

// ====== 6. 回收站操作（共享核心逻辑） ======

pub(crate) async fn delete_to_trash(
    pool: &sqlx::SqlitePool,
    username: &str,
    parent_path: &str,
    name: &str,
) -> Result<(), AppError> {
    use uuid::Uuid;
    let full = logical_path(parent_path, name);
    let base = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let physical = safe_join_sandbox(base, &user_file_path(username, parent_path, name))?;

    if physical.exists() {
        let trash_uuid = Uuid::new_v4().to_string();
        let trash_dir = std::path::Path::new(crate::constants::TRASH_DIR);
        let _ = tokio::fs::create_dir_all(trash_dir).await;

        // 意图日志先行（M1）：崩溃后启动重放补齐 trash 行 + files 行删除
        let jid =
            crate::handlers::journal::insert(pool, username, "trash", &full, &trash_uuid).await?;

        if let Err(e) = tokio::fs::rename(&physical, trash_dir.join(&trash_uuid)).await {
            crate::handlers::journal::remove(pool, jid).await;
            tracing::error!("[Files] 回收失败: {}", e);
            return Err(AppError::internal("操作失败"));
        }

        let db_result = async {
            let _ = sqlx::query(
                "INSERT INTO trash (username, original_path, trash_uuid) VALUES (?, ?, ?)",
            )
            .bind(username)
            .bind(&full)
            .bind(&trash_uuid)
            .execute(pool)
            .await?;
            db_delete_file_rows(pool, username, parent_path, name).await
        }
        .await;

        if let Err(e) = db_result {
            // 回滚物理移动 + 清除日志（不留下半截状态）
            let _ = std::fs::rename(trash_dir.join(&trash_uuid), &physical);
            crate::handlers::journal::remove(pool, jid).await;
            return Err(e);
        }
        crate::handlers::journal::remove(pool, jid).await;
    } else {
        // 物理不存在：仅清理 DB 行（纯 DB 操作，天然原子，无需日志）
        db_delete_file_rows(pool, username, parent_path, name).await?;
    }
    Ok(())
}

/// 删除 files 表中的目标行及其子路径行。
/// 子路径前缀必须 escape_like 转义（配合 ESCAPE '\'）——文件名含 %/_ 时
/// 历史实现会误删兄弟目录的 DB 行（M5 修复）。
pub(crate) async fn db_delete_file_rows(
    pool: &sqlx::SqlitePool,
    username: &str,
    parent_path: &str,
    name: &str,
) -> Result<(), AppError> {
    use crate::db::queries::escape_like;
    sqlx::query("DELETE FROM files WHERE username = ? AND name = ? AND parent_path = ?")
        .bind(username)
        .bind(name)
        .bind(parent_path)
        .execute(pool)
        .await?;
    let child_prefix = if parent_path.is_empty() {
        format!("{}/%", escape_like(name))
    } else {
        format!("{}/{}/%", escape_like(parent_path), escape_like(name))
    };
    let _ = sqlx::query("DELETE FROM files WHERE username = ? AND parent_path LIKE ? ESCAPE '\\'")
        .bind(username)
        .bind(&child_prefix)
        .execute(pool)
        .await;
    Ok(())
}

#[tracing::instrument(skip_all)]
pub async fn delete_item(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<DeleteRequest>,
) -> AppResult<(StatusCode, &'static str)> {
    let parent = user_dir_path(payload.current_path);
    let full = logical_path(&parent, &payload.name);
    let base = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let physical = safe_join_sandbox(
        base,
        &user_file_path(&session.username, &parent, &payload.name),
    )?;
    if !physical.exists() {
        return Err(AppError::not_found("目标实体不存在"));
    }
    delete_to_trash(&pool, &session.username, &parent, &payload.name).await?;
    let _ = update_user_used_mb(&pool, &session.username).await;
    let _ = log_audit(
        &pool,
        &session.username,
        "delete",
        Some(&full),
        None,
        None,
        None,
    )
    .await;
    Ok((StatusCode::OK, "已成功移至回收站"))
}

#[tracing::instrument(skip_all)]
pub async fn delete_batch(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<BatchDeleteRequest>,
) -> AppResult<(StatusCode, &'static str)> {
    let parent = user_dir_path(payload.current_path);
    let mut failed: Vec<String> = Vec::new();
    for name in &payload.names {
        if let Err(e) = delete_to_trash(&pool, &session.username, &parent, name).await {
            tracing::warn!("[Files] 批量删除 '{}' 失败: {}", name, e);
            failed.push(name.clone());
        }
    }
    let _ = update_user_used_mb(&pool, &session.username).await;
    let _ = log_audit(
        &pool,
        &session.username,
        "delete_batch",
        Some(&format!(
            "{} ok, {} failed",
            payload.names.len() - failed.len(),
            failed.len()
        )),
        None,
        None,
        None,
    )
    .await;
    if !failed.is_empty() {
        // 部分失败必须如实报告（此前一律"批量删除成功"掩盖失败）
        return Err(AppError::bad_request(format!(
            "{} 项删除失败: {}",
            failed.len(),
            failed.join(", ")
        )));
    }
    Ok((StatusCode::OK, "批量删除成功"))
}

// ====== Template Structs ======

#[derive(Template)]
#[template(path = "components/file_table.html")]
struct FileTableFragment {
    files: Vec<FileRowData>,
    current_path: String,
    /// 全局搜索模式（path 为空 + 有搜索词）：结果跨目录，显示所在路径
    has_global_search: bool,
    /// 当前搜索词（排序表头 hx-vals 需带上，否则点击排序即丢失搜索上下文）
    search: String,
}

struct FileRowData {
    name: String,
    size_display: String,
    is_dir: bool,
    /// 所在逻辑路径（正常浏览 = 当前目录；全局搜索 = 各自所在目录）
    parent_path: String,
}

struct BreadcrumbPart {
    name: String,
    path: String,
}

#[derive(Template)]
#[template(path = "components/breadcrumbs.html")]
struct BreadcrumbsFragment {
    parts: Vec<BreadcrumbPart>,
}

#[derive(Template)]
#[template(path = "components/upload_form.html")]
struct UploadFormFragment {
    current_path: String,
}

#[derive(Template)]
#[template(path = "components/new_folder_form.html")]
struct NewFolderFormFragment {}

#[derive(Template)]
#[template(path = "components/quota_bar.html")]
struct QuotaFragment {
    used_mb: i64,
    total_mb: i64,
    percent: u32,
}

#[derive(Template)]
#[template(path = "components/rename_form.html")]
struct RenameFormFragment {
    current_path: String,
    old_name: String,
}

#[derive(Template)]
#[template(path = "components/move_form.html")]
struct MoveFormFragment {
    current_path: String,
    name: String,
    dirs: Vec<String>,
}

#[derive(Template)]
#[template(path = "components/preview.html")]
pub struct PreviewFragment {
    file_name: String,
    file_path: String,
    file_size: String,
    mime_type: String,
    is_image: bool,
    is_video: bool,
    is_audio: bool,
    is_pdf: bool,
    is_text: bool,
    content: String,
    /// 用户(视频续播 localStorage key)
    username: String,
    /// Markdown 渲染模式：原始内容 JSON 编码（< 转义防 </script> 逃逸）
    is_markdown: bool,
    markdown_json: String,
    /// 画廊相邻文件（图片模式翻页；空串 = 无）
    prev_path: String,
    prev_name: String,
    next_path: String,
    next_name: String,
    /// 媒体令牌（短时效 + 目录限定）：<img>/<video> 等无 Cookie 场景的 /api/media/ 访问凭证
    media_token: String,
}

/// 画廊导航项（同目录相邻文件）
struct PreviewNav {
    path: String,
    name: String,
}

// ====== HTMX Fragment Handlers ======

/// GET /drive/list
#[tracing::instrument(skip_all)]
pub async fn drive_list_fragment(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let raw = params
        .get("path")
        .cloned()
        .unwrap_or_else(|| "/".to_string());
    let path = normalize_display_path(&raw);
    // 全局搜索：path 为空 + 有搜索词 → 全库跨目录搜索（显示所在路径列）
    let has_global_search = raw.trim().is_empty()
        && params
            .get("search")
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
    let files = query_files(
        &pool,
        &session.username,
        &raw,
        params.get("search").map(|s| s.as_str()),
        params.get("sort_by").map(|s| s.as_str()),
    )
    .await;
    AppTemplate(FileTableFragment {
        files,
        current_path: path,
        has_global_search,
        search: params.get("search").cloned().unwrap_or_default(),
    })
}

/// GET /drive/breadcrumbs
#[tracing::instrument(skip_all)]
pub async fn drive_breadcrumbs_fragment(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let path = params
        .get("path")
        .cloned()
        .unwrap_or_else(|| "/".to_string());
    let mut parts = vec![BreadcrumbPart {
        name: "~".to_string(),
        path: "/".to_string(),
    }];
    if path != "/" {
        let mut acc = String::new();
        for seg in path.trim_matches('/').split('/').filter(|s| !s.is_empty()) {
            acc.push('/');
            acc.push_str(seg);
            parts.push(BreadcrumbPart {
                name: seg.to_string(),
                path: acc.clone(),
            });
        }
    }
    AppTemplate(BreadcrumbsFragment { parts })
}

/// GET /drive/upload-form
pub async fn drive_upload_form(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let raw = params
        .get("path")
        .cloned()
        .unwrap_or_else(|| "/".to_string());
    let path = normalize_display_path(&raw);
    AppTemplate(UploadFormFragment { current_path: path })
}

/// GET /drive/new-folder-form
pub async fn drive_new_folder_form() -> impl IntoResponse {
    AppTemplate(NewFolderFormFragment {})
}

/// GET /drive/quota
#[tracing::instrument(skip_all)]
pub async fn drive_quota_fragment(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> impl IntoResponse {
    let row = sqlx::query("SELECT used_mb, quota_mb FROM users WHERE username = ?")
        .bind(&session.username)
        .fetch_optional(&pool)
        .await
        .unwrap_or(None);
    let (used, total) = row.map_or((0, 0), |r| (r.get::<i64, _>(0), r.get::<i64, _>(1)));
    let percent = if total > 0 {
        ((used as f64 / total as f64) * 100.0).min(100.0) as u32
    } else {
        0
    };
    AppTemplate(QuotaFragment {
        used_mb: used,
        total_mb: total,
        percent,
    })
}

/// POST /drive/create-folder
#[tracing::instrument(skip_all)]
pub async fn drive_create_folder(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    axum::extract::Form(form): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let name = form
        .get("name")
        .cloned()
        .unwrap_or_default()
        .trim()
        .to_string();
    let raw = form
        .get("current_path")
        .cloned()
        .unwrap_or_else(|| "/".to_string());
    let display = normalize_display_path(&raw);
    let parent = user_dir_path(Some(raw));
    if name.is_empty() {
        return fallback_file_list_with_error(
            pool.clone(),
            session.username.clone(),
            display.clone(),
            "文件夹名不能为空",
        )
        .await;
    }
    let target = match create_folder_common(
        &session.username,
        &parent,
        &name,
        std::path::Path::new(crate::constants::UPLOADS_DIR),
    ) {
        Ok(p) => p,
        Err(_) => {
            return fallback_file_list_with_error(
                pool.clone(),
                session.username.clone(),
                display.clone(),
                "文件夹名包含非法字符",
            )
            .await;
        }
    };
    // M11 一致性：INSERT 先行——UNIQUE 冲突即同名（绝不删除他人目录），成功后再建物理目录
    match sqlx::query("INSERT INTO files (username, name, parent_path, is_dir) VALUES (?, ?, ?, 1)")
        .bind(&session.username)
        .bind(&name)
        .bind(&parent)
        .execute(&pool)
        .await
    {
        Ok(_) => {}
        Err(e)
            if e.as_database_error()
                .is_some_and(|d| d.is_unique_violation()) =>
        {
            return fallback_file_list_with_error(
                pool.clone(),
                session.username.clone(),
                display.clone(),
                "同名文件夹已存在",
            )
            .await;
        }
        Err(e) => {
            tracing::error!("[Drive] 文件夹行登记失败: {}", e);
            return fallback_file_list_with_error(
                pool.clone(),
                session.username.clone(),
                display.clone(),
                "创建文件夹失败",
            )
            .await;
        }
    }
    if let Err(e) = tokio::fs::create_dir_all(&target).await {
        tracing::error!("[Drive] 创建目录失败: {}", e);
        let _ = sqlx::query(
            "DELETE FROM files WHERE username = ? AND name = ? AND parent_path = ? AND is_dir = 1",
        )
        .bind(&session.username)
        .bind(&name)
        .bind(&parent)
        .execute(&pool)
        .await;
        return fallback_file_list_with_error(
            pool.clone(),
            session.username.clone(),
            display.clone(),
            "创建文件夹失败",
        )
        .await;
    }
    let _ = log_audit(
        &pool,
        &session.username,
        "create_folder",
        Some(&logical_path(&parent, &name)),
        None,
        None,
        None,
    )
    .await;
    fallback_file_list(pool.clone(), session.username.clone(), display.clone()).await
}

/// POST /drive/delete
#[tracing::instrument(skip_all)]
pub async fn drive_delete_item(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    axum::extract::Form(form): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let name = form.get("name").cloned().unwrap_or_default();
    let current_path = form
        .get("current_path")
        .cloned()
        .unwrap_or_else(|| "/".to_string());
    let parent = user_dir_path(Some(current_path.clone()));
    if name.is_empty() {
        return fallback_file_list(pool.clone(), session.username.clone(), current_path.clone())
            .await;
    }
    if let Err(e) = delete_to_trash(&pool, &session.username, &parent, &name).await {
        tracing::error!("[Drive] 删除失败: {}", e);
        return fallback_file_list_with_error(
            pool.clone(),
            session.username.clone(),
            current_path.clone(),
            "删除失败，请稍后重试",
        )
        .await;
    }
    let _ = update_user_used_mb(&pool, &session.username).await;
    let _ = log_audit(
        &pool,
        &session.username,
        "delete",
        Some(&logical_path(&parent, &name)),
        None,
        None,
        None,
    )
    .await;
    fallback_file_list(pool.clone(), session.username.clone(), current_path.clone()).await
}

/// GET /drive/rename-form
pub async fn drive_rename_form(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    AppTemplate(RenameFormFragment {
        current_path: params
            .get("current_path")
            .cloned()
            .unwrap_or_else(|| "/".to_string()),
        old_name: params.get("name").cloned().unwrap_or_default(),
    })
}

/// POST /drive/rename
#[tracing::instrument(skip_all)]
pub async fn drive_rename_item(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    axum::extract::Form(form): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let current_path = form
        .get("current_path")
        .cloned()
        .unwrap_or_else(|| "/".to_string());
    let old_name = form.get("old_name").cloned().unwrap_or_default();
    let new_name = form
        .get("new_name")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let parent = current_path.trim_start_matches('/');
    if old_name.is_empty() || new_name.is_empty() || old_name == new_name {
        return fallback_file_list(pool.clone(), session.username.clone(), current_path.clone())
            .await;
    }
    if let Err(e) = rename_core(&pool, &session.username, parent, &old_name, &new_name).await {
        tracing::error!("[Drive] 重命名失败: {}", e);
        let msg = match e {
            AppError::Conflict(m) | AppError::BadRequest(m) => m,
            _ => "重命名失败，请稍后重试".to_string(),
        };
        return fallback_file_list_with_error(
            pool.clone(),
            session.username.clone(),
            current_path.clone(),
            &msg,
        )
        .await;
    }
    let _ = log_audit(
        &pool,
        &session.username,
        "rename",
        Some(&logical_path(parent, &old_name)),
        Some(&format!("-> {}", new_name)),
        None,
        None,
    )
    .await;
    fallback_file_list(pool.clone(), session.username.clone(), current_path.clone()).await
}

/// GET /drive/move-form
#[tracing::instrument(skip_all)]
pub async fn drive_move_form(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let current_path = params
        .get("current_path")
        .cloned()
        .unwrap_or_else(|| "/".to_string());
    let name = params.get("name").cloned().unwrap_or_default();
    #[derive(sqlx::FromRow)]
    struct D {
        parent_path: String,
        name: String,
    }
    let rows: Vec<D> = sqlx::query_as("SELECT parent_path, name FROM files WHERE username = ? AND is_dir = 1 ORDER BY parent_path, name")
        .bind(&session.username).fetch_all(&pool).await.unwrap_or_default();
    let mut dirs: Vec<String> = rows
        .iter()
        .map(|r| logical_path(&r.parent_path, &r.name))
        .collect();
    let source = current_path.trim_start_matches('/');
    let source_full = logical_path(source, &name);
    let prefix = format!("{}/", source_full);
    dirs.retain(|p| {
        let p = p.trim_start_matches('/');
        p != source_full && !p.starts_with(&prefix)
    });
    dirs.sort();
    AppTemplate(MoveFormFragment {
        current_path,
        name,
        dirs,
    })
}

/// POST /drive/move
#[tracing::instrument(skip_all)]
pub async fn drive_move_item(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    axum::extract::Form(form): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let current_path = form
        .get("current_path")
        .cloned()
        .unwrap_or_else(|| "/".to_string());
    let name = form.get("name").cloned().unwrap_or_default();
    let target_dir = form.get("target_dir").cloned().unwrap_or_default();
    let src = current_path.trim_start_matches('/');
    let dst = target_dir.trim_start_matches('/');
    if name.is_empty() || src == dst {
        return fallback_file_list(pool.clone(), session.username.clone(), current_path.clone())
            .await;
    }
    if let Err(e) = move_core(&pool, &session.username, src, dst, &name).await {
        tracing::error!("[Drive] 移动失败: {}", e);
        let msg = match e {
            AppError::Conflict(m) | AppError::BadRequest(m) => m,
            _ => "移动失败，请稍后重试".to_string(),
        };
        return fallback_file_list_with_error(
            pool.clone(),
            session.username.clone(),
            current_path.clone(),
            &msg,
        )
        .await;
    }
    let _ = log_audit(
        &pool,
        &session.username,
        "move",
        Some(&logical_path(src, &name)),
        Some(&format!("-> {}", dst)),
        None,
        None,
    )
    .await;
    fallback_file_list(pool.clone(), session.username.clone(), current_path.clone()).await
}

// ====== 文件预览 ======

const MAX_PREVIEW_TEXT_SIZE: u64 = 1024 * 1024;

/// GET /drive/preview
#[tracing::instrument(skip_all)]
pub async fn drive_preview(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> AppResult<AppTemplate<PreviewFragment>> {
    let current_path = params
        .get("path")
        .cloned()
        .unwrap_or_else(|| "/".to_string());
    let name = params.get("name").cloned().unwrap_or_default();
    let parent = current_path.trim_start_matches('/');
    let file_path = logical_path(parent, &name);
    let full_path = safe_join_sandbox(
        std::path::Path::new(crate::constants::UPLOADS_DIR),
        &user_file_path(&session.username, parent, &name),
    )?;

    let file_size = match tokio::fs::metadata(&full_path).await {
        Ok(meta) => bytes_to_mb_string(meta.len()),
        Err(_) => String::from("未知"),
    };

    let mime = normalize_preview_mime(&name, mime_guess::from_path(&name).first_or_octet_stream());
    let mime_str = mime.to_string();
    let is_image = mime.type_().as_str() == "image";
    let is_video = mime.type_().as_str() == "video";
    let is_audio = mime.type_().as_str() == "audio";
    let is_pdf = mime.subtype().as_str() == "pdf" || name.to_lowercase().ends_with(".pdf");
    let is_markdown = ["md", "markdown", "mdx"]
        .iter()
        .any(|e| name.to_lowercase().ends_with(&format!(".{}", e)));

    let text_mimes = [
        "text",
        "application/json",
        "application/xml",
        "application/javascript",
        "application/x-sh",
        "application/x-python",
    ];
    let text_exts = [
        "txt",
        "md",
        "rs",
        "py",
        "js",
        "ts",
        "html",
        "css",
        "json",
        "xml",
        "yaml",
        "yml",
        "toml",
        "ini",
        "cfg",
        "conf",
        "sh",
        "bash",
        "zsh",
        "sql",
        "log",
        "env",
        "c",
        "cpp",
        "h",
        "hpp",
        "java",
        "go",
        "rb",
        "php",
        "swift",
        "kt",
        "scala",
        "r",
        "lua",
        "vim",
        "dockerfile",
        "makefile",
        "editorconfig",
        "gitignore",
    ];
    let is_text = text_mimes.iter().any(|t| mime_str.starts_with(t))
        || text_exts.iter().any(|e| {
            name.to_lowercase().ends_with(&format!(".{}", e)) || name.to_lowercase() == *e
        });

    let content = if is_text || is_markdown {
        tokio::fs::read_to_string(&full_path)
            .await
            .map(|s| {
                if s.len() as u64 > MAX_PREVIEW_TEXT_SIZE {
                    format!(
                        "[文件过大，仅显示前 1 MB]\n\n{}",
                        s.chars()
                            .take(MAX_PREVIEW_TEXT_SIZE as usize)
                            .collect::<String>()
                    )
                } else {
                    s
                }
            })
            .unwrap_or_default()
    } else {
        String::new()
    };

    // Markdown 原文 JSON 编码后嵌入 script 数据块（< → < 防 </script> 逃逸；前端 marked+DOMPurify 渲染）
    let markdown_json = if is_markdown {
        serde_json::to_string(&content)
            .unwrap_or_default()
            .replace('<', "\\u003c")
    } else {
        String::new()
    };
    // 图片画廊：同目录相邻文件（按文件名排序）
    let (prev, next) = if is_image {
        gallery_neighbors(&pool, &session.username, parent, &name).await
    } else {
        (None, None)
    };
    let (prev_path, prev_name) = match prev {
        Some(p) => (p.path, p.name),
        None => (String::new(), String::new()),
    };
    let (next_path, next_name) = match next {
        Some(p) => (p.path, p.name),
        None => (String::new(), String::new()),
    };

    // 媒体类型签发短时效目录限定令牌（img/video/audio/iframe 无法带 Cookie/Authorization）
    let media_token = if is_image || is_video || is_audio || is_pdf {
        crate::handlers::utils::issue_media_token(&pool, &session.username, parent).await
    } else {
        String::new()
    };

    Ok(AppTemplate(PreviewFragment {
        file_name: name,
        file_path,
        file_size,
        mime_type: mime_str,
        is_image,
        is_video,
        is_audio,
        is_pdf,
        is_text,
        content,
        username: session.username,
        is_markdown,
        markdown_json,
        prev_path,
        prev_name,
        next_path,
        next_name,
        media_token,
    }))
}

/// 同目录相邻文件（忽略目录，按文件名 NOCASE 排序）——图片画廊翻页
async fn gallery_neighbors(
    pool: &sqlx::SqlitePool,
    username: &str,
    parent_path: &str,
    name: &str,
) -> (Option<PreviewNav>, Option<PreviewNav>) {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT name, parent_path FROM files WHERE username = ? AND parent_path = ? AND is_dir = 0 \
         ORDER BY name COLLATE NOCASE",
    )
    .bind(username)
    .bind(parent_path)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let Some(i) = rows.iter().position(|(n, _)| n == name) else {
        return (None, None);
    };
    let prev = rows
        .get(i.wrapping_sub(1))
        .filter(|_| i > 0)
        .map(|(n, p)| PreviewNav {
            path: p.clone(),
            name: n.clone(),
        });
    let next = rows.get(i + 1).map(|(n, p)| PreviewNav {
        path: p.clone(),
        name: n.clone(),
    });
    (prev, next)
}
