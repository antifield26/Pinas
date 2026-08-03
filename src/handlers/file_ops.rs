use axum::{
    extract::{Extension, Query},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Row};

use crate::error::{AppError, AppResult};
use crate::handlers::utils::{safe_join_sandbox, user_dir_path, update_user_used_mb, log_audit, bytes_to_mb_string};
use crate::db::queries::update_child_paths as update_child_parent_paths;
use askama::Template;
use crate::templates::AppTemplate;
use pinas_core::UserSession;

// ====== DTOs ======

#[derive(Deserialize)]
pub struct CreateFolderRequest { pub name: String, pub current_path: Option<String> }

#[derive(Deserialize)]
pub struct RenameRequest { pub name: String, pub new_name: String, pub current_path: Option<String> }

#[derive(Deserialize)]
pub struct MoveRequest { pub name: String, pub target_dir: String, pub current_path: Option<String> }

#[derive(Deserialize)]
pub struct MoveBatchRequest { pub names: Vec<String>, pub current_path: Option<String>, pub target_path: String }

#[derive(Deserialize, Debug)]
pub struct ListFilesQuery { pub path: Option<String>, pub search: Option<String>, pub sort_by: Option<String>, pub page: Option<i64>, pub page_size: Option<i64> }

#[derive(Serialize, FromRow)]
pub struct FileItem { pub id: i64, pub name: String, pub parent_path: String, pub is_dir: i64, pub size_mb: Option<f64>, pub created_at: String }

#[derive(Deserialize)]
pub struct DeleteRequest { pub name: String, pub current_path: Option<String> }

#[derive(Deserialize)]
pub struct BatchDeleteRequest { pub names: Vec<String>, pub current_path: Option<String> }

// ====== 公共 Helper 函数 ======

/// 构建用户文件的物理路径: `uploads/{username}/{parent}/{name}`
fn user_file_path(username: &str, parent: &str, name: &str) -> String {
    if parent.is_empty() { format!("{}/{}", username, name) }
    else { format!("{}/{}/{}", username, parent, name) }
}

/// 构建完整的目录路径: `{parent}/{name}`，parent 为空时返回 name
fn logical_path(parent: &str, name: &str) -> String {
    if parent.is_empty() { name.to_string() }
    else { format!("{}/{}", parent, name) }
}

/// 格式化文件大小用于显示（统一入口）
fn fmt_size(mb: f64, is_dir: bool) -> String {
    if is_dir { return String::from("--"); }
    if mb <= 0.0 { String::from("0 KB") }
    else if mb < 0.001 { format!("{:.2} KB", mb * 1024.0) }
    else if mb < 1.0 { format!("{:.1} KB", mb * 1024.0) }
    else { format!("{:.1} MB", mb) }
}

/// 磁盘文件存在性校验 + 孤儿 DB 记录清理
async fn ensure_file_on_disk(pool: &sqlx::SqlitePool, username: &str, parent_path: &str, name: &str, is_dir: bool) -> bool {
    let base = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let rel = user_file_path(username, parent_path, name);
    let full = safe_join_sandbox(base, &rel);
    let exists = if is_dir { full.is_dir() } else { full.exists() };
    if !exists {
        tracing::warn!("[文件同步] 磁盘文件缺失，清理数据库记录: {:?}", full);
        let _ = sqlx::query("DELETE FROM files WHERE username = ? AND name = ? AND parent_path = ?")
            .bind(username).bind(name).bind(parent_path).execute(pool).await;
        return false;
    }
    true
}

/// 标准化显示路径
fn normalize_display_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" { return "/".to_string(); }
    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    format!("/{}", segments.join("/"))
}

/// MIME 类型规范化（浏览器兼容）
fn normalize_preview_mime(file_name: &str, default_mime: mime_guess::Mime) -> mime_guess::Mime {
    let ext = std::path::Path::new(file_name).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
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

// ====== 文件列表查询 ======

/// 查询文件列表并过滤磁盘缺失项
async fn query_files(pool: &sqlx::SqlitePool, username: &str, path: &str, search: Option<&str>, sort_by: Option<&str>) -> Vec<FileRowData> {
    let parent_path = path.trim_start_matches('/');
    let mut qb = sqlx::QueryBuilder::new("SELECT name, size_mb, is_dir FROM files WHERE username = ");
    qb.push_bind(username);
    qb.push(" AND parent_path = ").push_bind(parent_path);
    if let Some(s) = search { if !s.is_empty() { qb.push(" AND name LIKE ").push_bind(format!("%{}%", s)); } }
    let order = match sort_by.unwrap_or("") {
        "name_desc" => " ORDER BY is_dir DESC, name DESC", "size_asc" => " ORDER BY is_dir DESC, size_mb ASC",
        "size_desc" => " ORDER BY is_dir DESC, size_mb DESC", "time_desc" => " ORDER BY is_dir DESC, created_at DESC",
        "time_asc" => " ORDER BY is_dir DESC, created_at ASC", _ => " ORDER BY is_dir DESC, name ASC",
    };
    qb.push(order);

    #[derive(sqlx::FromRow)]
    struct FileRowRaw { name: String, size_mb: Option<f64>, is_dir: i64 }
    match qb.build_query_as::<FileRowRaw>().fetch_all(pool).await {
        Ok(rows) => {
            let mut files = Vec::with_capacity(rows.len());
            for r in rows {
                let is_dir = r.is_dir != 0;
                if !ensure_file_on_disk(pool, username, parent_path, &r.name, is_dir).await { continue; }
                files.push(FileRowData { name: r.name, is_dir, size_display: fmt_size(r.size_mb.unwrap_or(0.0), is_dir) });
            }
            files
        }
        Err(e) => { tracing::error!("[Drive] 文件列表查询失败: {}", e); Vec::new() }
    }
}

/// HTMX 错误恢复：重新查询文件列表并返回模板，同时触发配额刷新
async fn fallback_file_list(pool: sqlx::SqlitePool, username: String, path: String) -> impl IntoResponse {
    let files = query_files(&pool, &username, &path, None, None).await;
    let mut resp = AppTemplate(FileTableFragment { files, current_path: path }).into_response();
    resp.headers_mut().insert("HX-Trigger", axum::http::HeaderValue::from_static("quotaRefresh"));
    resp
}

// ====== 1. JSON API: 文件列表 ======

fn bind_list_where(qb: &mut sqlx::QueryBuilder<sqlx::Sqlite>, username: &str, current_path: &str, has_search: bool, search_pattern: &str, like_pattern: &Option<String>) {
    qb.push(" WHERE username = ").push_bind(username);
    if has_search {
        if current_path.is_empty() {
            qb.push(" AND name LIKE ").push_bind(search_pattern);
        } else {
            qb.push(" AND (parent_path = ").push_bind(current_path);
            qb.push(" OR parent_path LIKE ").push_bind(like_pattern.as_deref().unwrap_or(""));
            qb.push(") AND name LIKE ").push_bind(search_pattern);
        }
    } else {
        qb.push(" AND parent_path = ").push_bind(current_path);
    }
}

#[tracing::instrument(skip_all)]
pub async fn list_files(
    Extension(pool): Extension<sqlx::SqlitePool>, Extension(session): Extension<UserSession>,
    Query(query): Query<ListFilesQuery>,
) -> impl IntoResponse {
    let current_path = user_dir_path(query.path);
    let has_search = query.search.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false);
    let search_pattern = if has_search { format!("%{}%", query.search.as_deref().unwrap_or("")) } else { String::new() };
    let like_pattern = if has_search && !current_path.is_empty() { Some(format!("{}/%", current_path)) } else { None };
    let page = query.page.unwrap_or(1).max(1);
    let page_size = query.page_size.unwrap_or(crate::constants::DEFAULT_PAGE_SIZE).min(crate::constants::MAX_PAGE_SIZE);
    let offset = (page - 1) * page_size;
    let order_sql = match query.sort_by.as_deref() {
        Some("name_desc") => " ORDER BY is_dir DESC, name DESC", Some("size_asc") => " ORDER BY is_dir DESC, size_mb ASC",
        Some("size_desc") => " ORDER BY is_dir DESC, size_mb DESC", Some("time_desc") => " ORDER BY is_dir DESC, created_at DESC",
        Some("time_asc") => " ORDER BY is_dir DESC, created_at ASC", _ => " ORDER BY is_dir DESC, name ASC",
    };

    let total: i64 = {
        let mut cq = sqlx::QueryBuilder::new("SELECT COUNT(*) FROM files");
        bind_list_where(&mut cq, &session.username, &current_path, has_search, &search_pattern, &like_pattern);
        cq.build_query_scalar().fetch_one(&pool).await.unwrap_or(0)
    };

    let mut dq = sqlx::QueryBuilder::new("SELECT id, name, parent_path, is_dir, size_mb, created_at FROM files");
    bind_list_where(&mut dq, &session.username, &current_path, has_search, &search_pattern, &like_pattern);
    dq.push(order_sql).push(" LIMIT ").push_bind(page_size).push(" OFFSET ").push_bind(offset);

    match dq.build_query_as::<FileItem>().fetch_all(&pool).await {
        Ok(files) => {
            let mut valid = Vec::with_capacity(files.len());
            for f in files {
                if ensure_file_on_disk(&pool, &session.username, &current_path, &f.name, f.is_dir != 0).await { valid.push(f); }
            }
            Json(serde_json::json!({"items": valid, "page": page, "page_size": page_size, "total": total, "total_pages": ((total as f64)/(page_size as f64)).ceil() as i64})).into_response()
        }
        Err(e) => { tracing::error!("[Files] 列表查询失败: {}", e); (StatusCode::INTERNAL_SERVER_ERROR, "读取文件列表失败").into_response() }
    }
}

// ====== 2. 创建文件夹 ======

fn create_folder_common(username: &str, parent: &str, name: &str, base: &std::path::Path) -> std::path::PathBuf {
    let sub = if parent.is_empty() { name.to_string() } else { format!("{}/{}", parent, name) };
    safe_join_sandbox(base, &format!("{}/{}", username, sub))
}

#[tracing::instrument(skip_all)]
pub async fn create_folder(
    Extension(pool): Extension<sqlx::SqlitePool>, Extension(session): Extension<UserSession>,
    Json(payload): Json<CreateFolderRequest>,
) -> AppResult<(StatusCode, &'static str)> {
    let name = payload.name.trim().to_string();
    if name.is_empty() { return Err(AppError::bad_request("名称不能为空")); }
    let parent = user_dir_path(payload.current_path);
    let target = create_folder_common(&session.username, &parent, &name, std::path::Path::new(crate::constants::UPLOADS_DIR));
    tokio::fs::create_dir_all(&target).await
        .map_err(|e| { tracing::error!("[Files] 创建目录失败: {}", e); AppError::internal("操作失败") })?;
    sqlx::query("INSERT INTO files (username, name, parent_path, is_dir) VALUES (?, ?, ?, 1)")
        .bind(&session.username).bind(&name).bind(&parent).execute(&pool).await?;
    let _ = log_audit(&pool, &session.username, "create_folder", Some(&logical_path(&parent, &name)), None, None, None).await;
    Ok((StatusCode::OK, "文件夹创建成功"))
}

// ====== 3. 重命名（共享核心逻辑） ======

/// 核心重命名逻辑：文件系统 + 事务内数据库更新 + 子路径更新 + 失败回滚
async fn rename_core(pool: &sqlx::SqlitePool, username: &str, parent: &str, old_name: &str, new_name: &str) -> Result<(), String> {
    let base = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let old_p = safe_join_sandbox(base, &user_file_path(username, parent, old_name));
    let new_p = safe_join_sandbox(base, &user_file_path(username, parent, new_name));

    tokio::fs::rename(&old_p, &new_p).await.map_err(|e| format!("文件系统重命名失败: {}", e))?;

    let mut tx = pool.begin().await.map_err(|e| { let _ = tokio::fs::rename(&new_p, &old_p); format!("事务失败: {}", e) })?;

    sqlx::query("UPDATE files SET name = ? WHERE username = ? AND name = ? AND parent_path = ?")
        .bind(new_name).bind(username).bind(old_name).bind(parent)
        .execute(&mut *tx).await.map_err(|e| { rollback_rename(&old_p, &new_p); format!("数据库更新失败: {}", e) })?;

    update_child_parent_paths(&mut tx, username, &logical_path(parent, old_name), &logical_path(parent, new_name)).await
        .map_err(|e| { rollback_rename(&old_p, &new_p); format!("子路径更新失败: {}", e) })?;

    tx.commit().await.map_err(|e| { rollback_rename(&old_p, &new_p); format!("提交失败: {}", e) })?;
    Ok(())
}

#[tracing::instrument(skip_all)]
pub async fn rename_item(
    Extension(pool): Extension<sqlx::SqlitePool>, Extension(session): Extension<UserSession>,
    Json(payload): Json<RenameRequest>,
) -> impl IntoResponse {
    let parent = user_dir_path(payload.current_path);
    let old_path = logical_path(&parent, &payload.name);
    match rename_core(&pool, &session.username, &parent, &payload.name, &payload.new_name).await {
        Ok(()) => {
            let _ = log_audit(&pool, &session.username, "rename", Some(&old_path), Some(&format!("-> {}", payload.new_name)), None, None).await;
            (StatusCode::OK, "重命名成功").into_response()
        }
        Err(e) => { tracing::error!("[Files] 重命名失败: {}", e); (StatusCode::INTERNAL_SERVER_ERROR, "操作失败").into_response() }
    }
}

// ====== 4. 移动（共享核心逻辑） ======

async fn move_core(pool: &sqlx::SqlitePool, username: &str, src_parent: &str, dst_parent: &str, name: &str) -> Result<(), String> {
    let base = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let src_p = safe_join_sandbox(base, &user_file_path(username, src_parent, name));
    let dst_p = safe_join_sandbox(base, &user_file_path(username, dst_parent, name));

    tokio::fs::rename(&src_p, &dst_p).await.map_err(|e| format!("文件系统移动失败: {}", e))?;

    let mut tx = pool.begin().await.map_err(|e| { let _ = tokio::fs::rename(&dst_p, &src_p); format!("事务失败: {}", e) })?;

    sqlx::query("UPDATE files SET parent_path = ? WHERE username = ? AND name = ? AND parent_path = ?")
        .bind(dst_parent).bind(username).bind(name).bind(src_parent)
        .execute(&mut *tx).await.map_err(|e| { let _ = tokio::fs::rename(&dst_p, &src_p); format!("数据库更新失败: {}", e) })?;

    update_child_parent_paths(&mut tx, username, &logical_path(src_parent, name), &logical_path(dst_parent, name)).await
        .map_err(|e| { let _ = tokio::fs::rename(&dst_p, &src_p); format!("子路径更新失败: {}", e) })?;

    tx.commit().await.map_err(|e| { let _ = tokio::fs::rename(&dst_p, &src_p); format!("提交失败: {}", e) })?;
    Ok(())
}

#[tracing::instrument(skip_all)]
pub async fn move_item(
    Extension(pool): Extension<sqlx::SqlitePool>, Extension(session): Extension<UserSession>,
    Json(payload): Json<MoveRequest>,
) -> impl IntoResponse {
    let src = user_dir_path(payload.current_path);
    let dst = user_dir_path(Some(payload.target_dir));
    let old_path = logical_path(&src, &payload.name);
    match move_core(&pool, &session.username, &src, &dst, &payload.name).await {
        Ok(()) => {
            let _ = log_audit(&pool, &session.username, "move", Some(&old_path), Some(&format!("-> {}", dst)), None, None).await;
            (StatusCode::OK, "迁移路径成功").into_response()
        }
        Err(e) => { tracing::error!("[Files] 移动失败: {}", e); (StatusCode::INTERNAL_SERVER_ERROR, "操作失败").into_response() }
    }
}

// ====== 5. 批量移动 ======

#[tracing::instrument(skip_all)]
pub async fn move_batch(
    Extension(pool): Extension<sqlx::SqlitePool>, Extension(session): Extension<UserSession>,
    Json(payload): Json<MoveBatchRequest>,
) -> impl IntoResponse {
    let src = user_dir_path(payload.current_path);
    let dst = user_dir_path(Some(payload.target_path));
    let base = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let mut moved: Vec<(String, std::path::PathBuf, std::path::PathBuf)> = Vec::new();

    for name in &payload.names {
        let sp = safe_join_sandbox(base, &user_file_path(&session.username, &src, name));
        let dp = safe_join_sandbox(base, &user_file_path(&session.username, &dst, name));
        if let Err(e) = tokio::fs::rename(&sp, &dp).await {
            for (_, s, d) in moved.iter().rev() { let _ = tokio::fs::rename(d, s).await; }
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("移动 '{}' 失败: {}", name, e)).into_response();
        }
        moved.push((name.clone(), sp, dp));
    }

    let mut tx = match pool.begin().await {
        Ok(tx) => tx, Err(e) => { for (_, s, d) in moved.iter().rev() { let _ = tokio::fs::rename(d, s); } return (StatusCode::INTERNAL_SERVER_ERROR, format!("事务失败: {}", e)).into_response(); }
    };

    for (name, _, _) in &moved {
        if let Err(e) = sqlx::query("UPDATE files SET parent_path = ? WHERE username = ? AND name = ? AND parent_path = ?")
            .bind(&dst).bind(&session.username).bind(name).bind(&src).execute(&mut *tx).await {
            for (_, s, d) in moved.iter().rev() { let _ = tokio::fs::rename(d, s); }
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("更新失败: {}", e)).into_response();
        }
        if let Err(e) = update_child_parent_paths(&mut tx, &session.username, &logical_path(&src, name), &logical_path(&dst, name)).await {
            for (_, s, d) in moved.iter().rev() { let _ = tokio::fs::rename(d, s); }
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("子路径更新失败: {}", e)).into_response();
        }
    }
    if let Err(e) = tx.commit().await { for (_, s, d) in moved.iter().rev() { let _ = tokio::fs::rename(d, s); } return (StatusCode::INTERNAL_SERVER_ERROR, format!("提交失败: {}", e)).into_response(); }

    let _ = log_audit(&pool, &session.username, "move_batch", Some(&format!("[{}]", payload.names.join(","))), Some(&format!("{} -> {}", src, dst)), None, None).await;
    (StatusCode::OK, "批量移动成功").into_response()
}

// ====== 6. 回收站操作（共享核心逻辑） ======

async fn delete_to_trash(pool: &sqlx::SqlitePool, username: &str, parent_path: &str, name: &str) -> Result<(), AppError> {
    use uuid::Uuid;
    let full = logical_path(parent_path, name);
    let base = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let physical = safe_join_sandbox(base, &user_file_path(username, parent_path, name));

    if physical.exists() {
        let trash_uuid = Uuid::new_v4().to_string();
        let trash_dir = std::path::Path::new(crate::constants::TRASH_DIR);
        let _ = tokio::fs::create_dir_all(trash_dir).await;
        tokio::fs::rename(&physical, trash_dir.join(&trash_uuid)).await
            .map_err(|e| { tracing::error!("[Files] 回收失败: {}", e); AppError::internal("操作失败") })?;
        let _ = sqlx::query("INSERT INTO trash (username, original_path, trash_uuid) VALUES (?, ?, ?)")
            .bind(username).bind(&full).bind(&trash_uuid).execute(pool).await;
    }
    // 清理 DB 记录（目录子树一并清理）
    sqlx::query("DELETE FROM files WHERE username = ? AND name = ? AND parent_path = ?")
        .bind(username).bind(name).bind(parent_path).execute(pool).await?;
    let child_prefix = if parent_path.is_empty() { format!("{}/%", name) } else { format!("{}/{}/%", parent_path, name) };
    let _ = sqlx::query("DELETE FROM files WHERE username = ? AND parent_path LIKE ?")
        .bind(username).bind(&child_prefix).execute(pool).await;
    Ok(())
}

#[tracing::instrument(skip_all)]
pub async fn delete_item(
    Extension(pool): Extension<sqlx::SqlitePool>, Extension(session): Extension<UserSession>,
    Json(payload): Json<DeleteRequest>,
) -> AppResult<(StatusCode, &'static str)> {
    let parent = user_dir_path(payload.current_path);
    let full = logical_path(&parent, &payload.name);
    let base = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let physical = safe_join_sandbox(base, &user_file_path(&session.username, &parent, &payload.name));
    if !physical.exists() { return Err(AppError::not_found("目标实体不存在")); }
    delete_to_trash(&pool, &session.username, &parent, &payload.name).await?;
    let _ = update_user_used_mb(&pool, &session.username).await;
    let _ = log_audit(&pool, &session.username, "delete", Some(&full), None, None, None).await;
    Ok((StatusCode::OK, "已成功移至回收站"))
}

#[tracing::instrument(skip_all)]
pub async fn delete_batch(
    Extension(pool): Extension<sqlx::SqlitePool>, Extension(session): Extension<UserSession>,
    Json(payload): Json<BatchDeleteRequest>,
) -> AppResult<(StatusCode, &'static str)> {
    let parent = user_dir_path(payload.current_path);
    for name in &payload.names {
        if delete_to_trash(&pool, &session.username, &parent, name).await.is_err() { continue; }
    }
    let _ = update_user_used_mb(&pool, &session.username).await;
    let _ = log_audit(&pool, &session.username, "delete_batch", Some(&format!("{} items", payload.names.len())), None, None, None).await;
    Ok((StatusCode::OK, "批量删除成功"))
}

// ====== Template Structs ======

#[derive(Template)] #[template(path = "components/file_table.html")]
struct FileTableFragment { files: Vec<FileRowData>, current_path: String }

struct FileRowData { name: String, size_display: String, is_dir: bool }

struct BreadcrumbPart { name: String, path: String }

#[derive(Template)] #[template(path = "components/breadcrumbs.html")]
struct BreadcrumbsFragment { parts: Vec<BreadcrumbPart> }

#[derive(Template)] #[template(path = "components/upload_form.html")]
struct UploadFormFragment { current_path: String }

#[derive(Template)] #[template(path = "components/new_folder_form.html")]
struct NewFolderFormFragment {}

#[derive(Template)] #[template(path = "components/quota_bar.html")]
struct QuotaFragment { used_mb: i64, total_mb: i64, percent: u32 }

#[derive(Template)] #[template(path = "components/rename_form.html")]
struct RenameFormFragment { current_path: String, old_name: String }

#[derive(Template)] #[template(path = "components/move_form.html")]
struct MoveFormFragment { current_path: String, name: String, dirs: Vec<String> }

#[derive(Template)] #[template(path = "components/preview.html")]
struct PreviewFragment { file_name: String, file_path: String, file_size: String, mime_type: String, is_image: bool, is_video: bool, is_audio: bool, is_pdf: bool, is_text: bool, content: String }

// ====== HTMX Fragment Handlers ======

/// GET /drive/list
#[tracing::instrument(skip_all)]
pub async fn drive_list_fragment(
    Extension(pool): Extension<sqlx::SqlitePool>, Extension(session): Extension<UserSession>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let raw = params.get("path").cloned().unwrap_or_else(|| "/".to_string());
    let path = normalize_display_path(&raw);
    let files = query_files(&pool, &session.username, &raw, params.get("search").map(|s| s.as_str()), params.get("sort_by").map(|s| s.as_str())).await;
    AppTemplate(FileTableFragment { files, current_path: path })
}

/// GET /drive/breadcrumbs
#[tracing::instrument(skip_all)]
pub async fn drive_breadcrumbs_fragment(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let path = params.get("path").cloned().unwrap_or_else(|| "/".to_string());
    let mut parts = vec![BreadcrumbPart { name: "~".to_string(), path: "/".to_string() }];
    if path != "/" {
        let mut acc = String::new();
        for seg in path.trim_matches('/').split('/').filter(|s| !s.is_empty()) {
            acc.push('/'); acc.push_str(seg);
            parts.push(BreadcrumbPart { name: seg.to_string(), path: acc.clone() });
        }
    }
    AppTemplate(BreadcrumbsFragment { parts })
}

/// GET /drive/upload-form
pub async fn drive_upload_form(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let raw = params.get("path").cloned().unwrap_or_else(|| "/".to_string());
    let path = normalize_display_path(&raw);
    AppTemplate(UploadFormFragment { current_path: path })
}

/// GET /drive/new-folder-form
pub async fn drive_new_folder_form() -> impl IntoResponse { AppTemplate(NewFolderFormFragment {}) }

/// GET /drive/quota
#[tracing::instrument(skip_all)]
pub async fn drive_quota_fragment(
    Extension(pool): Extension<sqlx::SqlitePool>, Extension(session): Extension<UserSession>,
) -> impl IntoResponse {
    let row = sqlx::query("SELECT used_mb, quota_mb FROM users WHERE username = ?")
        .bind(&session.username).fetch_optional(&pool).await.unwrap_or(None);
    let (used, total) = row.map_or((0, 0), |r| (r.get::<i64, _>(0), r.get::<i64, _>(1)));
    let percent = if total > 0 { ((used as f64 / total as f64) * 100.0).min(100.0) as u32 } else { 0 };
    AppTemplate(QuotaFragment { used_mb: used, total_mb: total, percent })
}

/// POST /drive/create-folder
#[tracing::instrument(skip_all)]
pub async fn drive_create_folder(
    Extension(pool): Extension<sqlx::SqlitePool>, Extension(session): Extension<UserSession>,
    axum::extract::Form(form): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let name = form.get("name").cloned().unwrap_or_default().trim().to_string();
    let raw = form.get("current_path").cloned().unwrap_or_else(|| "/".to_string());
    let display = normalize_display_path(&raw);
    let parent = user_dir_path(Some(raw));
    if name.is_empty() { return fallback_file_list(pool.clone(), session.username.clone(), display.clone()).await; }
    let target = create_folder_common(&session.username, &parent, &name, std::path::Path::new(crate::constants::UPLOADS_DIR));
    if let Err(e) = tokio::fs::create_dir_all(&target).await {
        tracing::error!("[Drive] 创建目录失败: {}", e);
        return fallback_file_list(pool.clone(), session.username.clone(), display.clone()).await;
    }
    let _ = sqlx::query("INSERT OR IGNORE INTO files (username, name, parent_path, is_dir) VALUES (?, ?, ?, 1)")
        .bind(&session.username).bind(&name).bind(&parent).execute(&pool).await;
    let _ = log_audit(&pool, &session.username, "create_folder", Some(&logical_path(&parent, &name)), None, None, None).await;
    fallback_file_list(pool.clone(), session.username.clone(), display.clone()).await
}

/// POST /drive/delete
#[tracing::instrument(skip_all)]
pub async fn drive_delete_item(
    Extension(pool): Extension<sqlx::SqlitePool>, Extension(session): Extension<UserSession>,
    axum::extract::Form(form): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let name = form.get("name").cloned().unwrap_or_default();
    let current_path = form.get("current_path").cloned().unwrap_or_else(|| "/".to_string());
    let parent = user_dir_path(Some(current_path.clone()));
    if name.is_empty() { return fallback_file_list(pool.clone(), session.username.clone(), current_path.clone()).await; }
    let _ = delete_to_trash(&pool, &session.username, &parent, &name).await;
    let _ = update_user_used_mb(&pool, &session.username).await;
    let _ = log_audit(&pool, &session.username, "delete", Some(&logical_path(&parent, &name)), None, None, None).await;
    fallback_file_list(pool.clone(), session.username.clone(), current_path.clone()).await
}

/// GET /drive/rename-form
pub async fn drive_rename_form(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    AppTemplate(RenameFormFragment {
        current_path: params.get("current_path").cloned().unwrap_or_else(|| "/".to_string()),
        old_name: params.get("name").cloned().unwrap_or_default(),
    })
}

/// POST /drive/rename
#[tracing::instrument(skip_all)]
pub async fn drive_rename_item(
    Extension(pool): Extension<sqlx::SqlitePool>, Extension(session): Extension<UserSession>,
    axum::extract::Form(form): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let current_path = form.get("current_path").cloned().unwrap_or_else(|| "/".to_string());
    let old_name = form.get("old_name").cloned().unwrap_or_default();
    let new_name = form.get("new_name").map(|s| s.trim().to_string()).unwrap_or_default();
    let parent = current_path.trim_start_matches('/');
    if old_name.is_empty() || new_name.is_empty() || old_name == new_name {
        return fallback_file_list(pool.clone(), session.username.clone(), current_path.clone()).await;
    }
    if let Err(e) = rename_core(&pool, &session.username, parent, &old_name, &new_name).await {
        tracing::error!("[Drive] 重命名失败: {}", e);
    } else {
        let _ = log_audit(&pool, &session.username, "rename", Some(&logical_path(parent, &old_name)), Some(&format!("-> {}", new_name)), None, None).await;
    }
    fallback_file_list(pool.clone(), session.username.clone(), current_path.clone()).await
}

/// GET /drive/move-form
#[tracing::instrument(skip_all)]
pub async fn drive_move_form(
    Extension(pool): Extension<sqlx::SqlitePool>, Extension(session): Extension<UserSession>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let current_path = params.get("current_path").cloned().unwrap_or_else(|| "/".to_string());
    let name = params.get("name").cloned().unwrap_or_default();
    #[derive(sqlx::FromRow)] struct D { parent_path: String, name: String }
    let rows: Vec<D> = sqlx::query_as("SELECT parent_path, name FROM files WHERE username = ? AND is_dir = 1 ORDER BY parent_path, name")
        .bind(&session.username).fetch_all(&pool).await.unwrap_or_default();
    let mut dirs: Vec<String> = rows.iter().map(|r| logical_path(&r.parent_path, &r.name)).collect();
    let source = current_path.trim_start_matches('/');
    let source_full = logical_path(source, &name);
    let prefix = format!("{}/", source_full);
    dirs.retain(|p| { let p = p.trim_start_matches('/'); p != source_full && !p.starts_with(&prefix) });
    dirs.sort();
    AppTemplate(MoveFormFragment { current_path, name, dirs })
}

/// POST /drive/move
#[tracing::instrument(skip_all)]
pub async fn drive_move_item(
    Extension(pool): Extension<sqlx::SqlitePool>, Extension(session): Extension<UserSession>,
    axum::extract::Form(form): axum::extract::Form<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let current_path = form.get("current_path").cloned().unwrap_or_else(|| "/".to_string());
    let name = form.get("name").cloned().unwrap_or_default();
    let target_dir = form.get("target_dir").cloned().unwrap_or_default();
    let src = current_path.trim_start_matches('/');
    let dst = target_dir.trim_start_matches('/');
    if name.is_empty() || src == dst { return fallback_file_list(pool.clone(), session.username.clone(), current_path.clone()).await; }
    if let Err(e) = move_core(&pool, &session.username, src, dst, &name).await {
        tracing::error!("[Drive] 移动失败: {}", e);
    } else {
        let _ = log_audit(&pool, &session.username, "move", Some(&logical_path(src, &name)), Some(&format!("-> {}", dst)), None, None).await;
    }
    fallback_file_list(pool.clone(), session.username.clone(), current_path.clone()).await
}

// ====== 文件预览 ======

const MAX_PREVIEW_TEXT_SIZE: u64 = 1 * 1024 * 1024;

/// GET /drive/preview
#[tracing::instrument(skip_all)]
pub async fn drive_preview(
    Extension(session): Extension<UserSession>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let current_path = params.get("path").cloned().unwrap_or_else(|| "/".to_string());
    let name = params.get("name").cloned().unwrap_or_default();
    let parent = current_path.trim_start_matches('/');
    let file_path = logical_path(parent, &name);
    let full_path = safe_join_sandbox(std::path::Path::new(crate::constants::UPLOADS_DIR), &user_file_path(&session.username, parent, &name));

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

    let text_mimes = ["text", "application/json", "application/xml", "application/javascript", "application/x-sh", "application/x-python"];
    let text_exts = ["txt","md","rs","py","js","ts","html","css","json","xml","yaml","yml","toml","ini","cfg","conf","sh","bash","zsh","sql","log","env","c","cpp","h","hpp","java","go","rb","php","swift","kt","scala","r","lua","vim","dockerfile","makefile","editorconfig","gitignore"];
    let is_text = text_mimes.iter().any(|t| mime_str.starts_with(t))
        || text_exts.iter().any(|e| name.to_lowercase().ends_with(&format!(".{}", e)) || name.to_lowercase() == *e);

    let content = if is_text {
        tokio::fs::read_to_string(&full_path).await.map(|s| {
            if s.len() as u64 > MAX_PREVIEW_TEXT_SIZE {
                format!("[文件过大，仅显示前 1 MB]\n\n{}", s.chars().take(MAX_PREVIEW_TEXT_SIZE as usize).collect::<String>())
            } else { s }
        }).unwrap_or_default()
    } else { String::new() };

    AppTemplate(PreviewFragment { file_name: name, file_path, file_size, mime_type: mime_str, is_image, is_video, is_audio, is_pdf, is_text, content })
}
