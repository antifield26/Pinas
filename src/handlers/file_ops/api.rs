// ====== file_ops：JSON API 处理器（P1-1 拆分） ======
use axum::{
    extract::{Extension, Query},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::core::UserSession;
use crate::db::queries::update_child_paths as update_child_parent_paths;
use crate::error::{AppError, AppResult};
use crate::handlers::file_ops::core::{
    bind_list_where, create_folder_common, delete_to_trash, logical_path, move_core,
    reconcile_files_on_disk, rename_core, rollback_batch_moves, user_file_path,
};
use crate::handlers::utils::{log_audit, safe_join_sandbox, update_user_used_mb, user_dir_path};

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
    // 沙箱 rel 路径（openat2 原子校验；create_folder_common 的字符串校验保留为纵深防御）
    let _target = create_folder_common(
        &session.username,
        &parent,
        &name,
        std::path::Path::new(crate::constants::UPLOADS_DIR),
    )?;
    let _rel = user_file_path(&session.username, &parent, &name);
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
    if let Err(e) = crate::fsutil::Sandbox::new(crate::constants::UPLOADS_DIR)
        .and_then(|sb| sb.create_dir_all(&user_file_path(&session.username, &parent, &name)))
    {
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

pub async fn rename_item(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<RenameRequest>,
) -> impl IntoResponse {
    if crate::handlers::utils::validate_name(&payload.new_name).is_err()
        || crate::handlers::utils::validate_name(&payload.name).is_err()
    {
        return (StatusCode::BAD_REQUEST, "名称包含非法字符").into_response();
    }
    // P0-1：parent 逐段白名单校验（拒绝 `..` 跨用户路径），失败直接 400
    let parent = match crate::handlers::utils::validate_rel_path(
        payload.current_path.as_deref().unwrap_or(""),
    ) {
        Ok(p) => p,
        Err(_) => return (StatusCode::BAD_REQUEST, "非法路径").into_response(),
    };
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
        Err(AppError::BadRequest(msg)) => (StatusCode::BAD_REQUEST, msg).into_response(),
        Err(e) => {
            tracing::error!("[Files] 重命名失败: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "操作失败").into_response()
        }
    }
}

// ====== 4. 移动（共享核心逻辑） ======

pub async fn move_item(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<MoveRequest>,
) -> impl IntoResponse {
    // P0-2：源名必须过白名单（`..` 源名可移走整棵用户子树）
    if crate::handlers::utils::validate_name(&payload.name).is_err() {
        return (StatusCode::BAD_REQUEST, "名称包含非法字符").into_response();
    }
    // P0-1：src/dst 逐段白名单校验（拒绝 `..` 跨用户路径）
    let src = match crate::handlers::utils::validate_rel_path(
        payload.current_path.as_deref().unwrap_or(""),
    ) {
        Ok(p) => p,
        Err(_) => return (StatusCode::BAD_REQUEST, "非法路径").into_response(),
    };
    let dst = match crate::handlers::utils::validate_rel_path(&payload.target_dir) {
        Ok(p) => p,
        Err(_) => return (StatusCode::BAD_REQUEST, "非法路径").into_response(),
    };
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
        Err(AppError::BadRequest(msg)) => (StatusCode::BAD_REQUEST, msg).into_response(),
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
    let sb = match crate::fsutil::Sandbox::new(crate::constants::UPLOADS_DIR) {
        Ok(s) => s,
        Err(_) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "操作失败，请稍后重试").into_response();
        }
    };
    let mut moved: Vec<(String, String, String)> = Vec::new();
    // 意图日志（M1）：每个条目在物理移动前落一条记录，崩溃后逐条重放
    let mut jids: Vec<i64> = Vec::new();

    for name in &payload.names {
        let sp = user_file_path(&session.username, &src, name);
        let dp = user_file_path(&session.username, &dst, name);
        // 字符串级纵深防御校验（openat2 在内核层兜底）
        if safe_join_sandbox(std::path::Path::new(crate::constants::UPLOADS_DIR), &sp).is_err()
            || safe_join_sandbox(std::path::Path::new(crate::constants::UPLOADS_DIR), &dp).is_err()
        {
            rollback_batch_moves(&sb, &moved);
            crate::handlers::journal::remove_many(&pool, &jids).await;
            return (StatusCode::BAD_REQUEST, "包含非法路径").into_response();
        }
        // 目标冲突预检：fs rename 覆盖已存在目标即永久销毁其内容（同 rename_core/move_core）
        if sb.try_exists(&dp).unwrap_or(true) {
            rollback_batch_moves(&sb, &moved);
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
                rollback_batch_moves(&sb, &moved);
                crate::handlers::journal::remove_many(&pool, &jids).await;
                return (StatusCode::INTERNAL_SERVER_ERROR, "操作失败，请稍后重试").into_response();
            }
        };
        jids.push(jid);
        if let Err(e) = sb.rename(&sp, &dp) {
            tracing::error!("[Files] 批量移动 '{}' 失败: {}", name, e);
            rollback_batch_moves(&sb, &moved);
            crate::handlers::journal::remove_many(&pool, &jids).await;
            return (StatusCode::INTERNAL_SERVER_ERROR, "移动失败，操作已回滚").into_response();
        }
        moved.push((name.clone(), sp, dp));
    }

    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            rollback_batch_moves(&sb, &moved);
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
            rollback_batch_moves(&sb, &moved);
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
            rollback_batch_moves(&sb, &moved);
            crate::handlers::journal::remove_many(&pool, &jids).await;
            tracing::error!("[Files] 批量移动子路径更新失败: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "操作失败，请稍后重试").into_response();
        }
    }
    if let Err(e) = tx.commit().await {
        rollback_batch_moves(&sb, &moved);
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

pub async fn delete_item(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<DeleteRequest>,
) -> AppResult<(StatusCode, &'static str)> {
    // P0-1：parent 逐段白名单校验（拒绝 `..` 跨用户路径）
    let parent =
        crate::handlers::utils::validate_rel_path(payload.current_path.as_deref().unwrap_or(""))?;
    let full = logical_path(&parent, &payload.name);
    let rel = user_file_path(&session.username, &parent, &payload.name);
    let sb = crate::fsutil::Sandbox::new(crate::constants::UPLOADS_DIR)
        .map_err(|e| AppError::internal_log("打开沙箱", e))?;
    if !sb.try_exists(&rel).unwrap_or(false) {
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
    // P0-1：parent 逐段白名单校验（拒绝 `..` 跨用户路径）
    let parent =
        crate::handlers::utils::validate_rel_path(payload.current_path.as_deref().unwrap_or(""))?;
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
