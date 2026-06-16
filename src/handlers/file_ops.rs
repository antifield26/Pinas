use axum::{
    extract::{Extension, Query},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::handlers::utils::{safe_join_sandbox, user_dir_path, update_user_used_mb, log_audit};
use pinas_core::UserSession;

// DTOs
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
    pub size_mb: String,
    pub created_at: String,
}

#[derive(Deserialize)]
pub struct DeleteRequest {
    pub name: String,
    pub current_path: Option<String>,
}

// 1. 获取文件列表（支持搜索、排序和分页）
pub async fn list_files(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Query(query): Query<ListFilesQuery>,
) -> impl IntoResponse {
    let username = session.username;
    let current_path = user_dir_path(query.path);
    let has_search = query.search.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false);
    let search_pattern = if has_search {
        format!("%{}%", query.search.as_ref().unwrap().trim())
    } else {
        String::new()
    };

    // 分页参数，默认 page=1, page_size=50
    let page = query.page.unwrap_or(1).max(1);
    let page_size = query.page_size.unwrap_or(crate::constants::DEFAULT_PAGE_SIZE).min(crate::constants::MAX_PAGE_SIZE);
    let offset = (page - 1) * page_size;

    // 构建 WHERE 条件
    let mut where_sql = String::from("WHERE username = ?");
    if has_search {
        if current_path.is_empty() {
            where_sql.push_str(" AND name LIKE ?");
        } else {
            where_sql.push_str(" AND (parent_path = ? OR parent_path LIKE ?) AND name LIKE ?");
        }
    } else {
        where_sql.push_str(" AND parent_path = ?");
    }

    // 排序
    let order_sql = match query.sort_by.as_deref() {
        Some("name_desc") => " ORDER BY is_dir DESC, name DESC",
        Some("size_asc") => " ORDER BY is_dir DESC, size_mb ASC",
        Some("size_desc") => " ORDER BY is_dir DESC, size_mb DESC",
        Some("time_desc") => " ORDER BY is_dir DESC, created_at DESC",
        Some("time_asc") => " ORDER BY is_dir DESC, created_at ASC",
        _ => " ORDER BY is_dir DESC, name ASC",
    };

    // 先查总数
    let count_sql = format!("SELECT COUNT(*) FROM files {}", where_sql);
    let like_pattern_opt = if has_search && !current_path.is_empty() {
        Some(format!("{}/%", current_path))
    } else {
        None
    };

    let total: i64 = {
        let mut cq = sqlx::query_scalar(&count_sql).bind(&username);
        if has_search {
            if current_path.is_empty() {
                cq = cq.bind(&search_pattern);
            } else {
                cq = cq.bind(&current_path)
                    .bind(like_pattern_opt.as_ref().unwrap())
                    .bind(&search_pattern);
            }
        } else {
            cq = cq.bind(&current_path);
        }
        cq.fetch_one(&pool).await.unwrap_or(0)
    };

    // 查数据
    let data_sql = format!(
        "SELECT id, name, parent_path, is_dir, size_mb, created_at FROM files {} {} LIMIT ? OFFSET ?",
        where_sql, order_sql
    );

    let mut db_query = sqlx::query_as::<_, FileItem>(&data_sql).bind(&username);

    if has_search {
        if current_path.is_empty() {
            db_query = db_query.bind(&search_pattern);
        } else {
            db_query = db_query
                .bind(&current_path)
                .bind(like_pattern_opt.as_ref().unwrap())
                .bind(&search_pattern);
        }
    } else {
        db_query = db_query.bind(&current_path);
    }

    db_query = db_query.bind(page_size).bind(offset);

    match db_query.fetch_all(&pool).await {
        Ok(files) => {
            let total_pages = ((total as f64) / (page_size as f64)).ceil() as i64;
            let response = serde_json::json!({
                "items": files,
                "page": page,
                "page_size": page_size,
                "total": total,
                "total_pages": total_pages
            });
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("读取文件列表失败: {}", e)).into_response(),
    }
}

// 2. 创建文件夹
pub async fn create_folder(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<CreateFolderRequest>,
) -> impl IntoResponse {
    let username = &session.username;
    let folder_name = payload.name.trim();
    if folder_name.is_empty() {
        return (StatusCode::BAD_REQUEST, "名称不能为空").into_response();
    }

    let parent_path = user_dir_path(payload.current_path);
    let base_path = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let user_dir = safe_join_sandbox(base_path, &format!("{}/{}", username, parent_path));
    let target_dir = user_dir.join(folder_name);

    if let Err(e) = tokio::fs::create_dir_all(&target_dir).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("物理目录创建失败: {}", e)).into_response();
    }

    let result = sqlx::query("INSERT INTO files (username, name, parent_path, is_dir, size_mb) VALUES (?, ?, ?, 1, '-')")
        .bind(username)
        .bind(folder_name)
        .bind(&parent_path)
        .execute(&pool)
        .await;

    match result {
        Ok(_) => {
            let target = if parent_path.is_empty() {
                folder_name.to_string()
            } else {
                format!("{}/{}", parent_path, folder_name)
            };
            let _ = log_audit(&pool, username, "create_folder", Some(&target), None, None, None).await;
            (StatusCode::OK, "文件夹创建成功").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("数据库录入失败: {}", e)).into_response(),
    }
}

// 3. 重命名（包括目录子树）
pub async fn rename_item(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<RenameRequest>,
) -> impl IntoResponse {
    let username = &session.username;
    let parent_path = user_dir_path(payload.current_path);
    let base_path = std::path::Path::new(crate::constants::UPLOADS_DIR);

    let old_p = safe_join_sandbox(base_path, &format!("{}/{}/{}", username, parent_path, payload.name));
    let new_p = safe_join_sandbox(base_path, &format!("{}/{}/{}", username, parent_path, payload.new_name));

    if let Err(e) = tokio::fs::rename(&old_p, &new_p).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("重命名物理媒介失败: {}", e)).into_response();
    }

    // 使用事务确保目录及其子记录的原子更新
    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            let _ = tokio::fs::rename(&new_p, &old_p).await;
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("开启事务失败: {}", e)).into_response();
        }
    };

    let old_dir_path = if parent_path.is_empty() { payload.name.clone() } else { format!("{}/{}", parent_path, payload.name) };
    let new_dir_path = if parent_path.is_empty() { payload.new_name.clone() } else { format!("{}/{}", parent_path, payload.new_name) };

    // 更新被重命名的条目本身（目录或文件）
    if let Err(e) = sqlx::query("UPDATE files SET name = ? WHERE username = ? AND name = ? AND parent_path = ?")
        .bind(&payload.new_name)
        .bind(username)
        .bind(&payload.name)
        .bind(&parent_path)
        .execute(&mut *tx)
        .await
    {
        let _ = tokio::fs::rename(&new_p, &old_p).await;
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("数据库更新失败: {}", e)).into_response();
    }

    // 如果是目录，更新所有子记录的 parent_path
    if let Err(e) = update_child_parent_paths(&mut tx, username, &old_dir_path, &new_dir_path).await {
        let _ = tokio::fs::rename(&new_p, &old_p).await;
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("更新子目录记录失败: {}", e)).into_response();
    }

    if let Err(e) = tx.commit().await {
        let _ = tokio::fs::rename(&new_p, &old_p).await;
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("提交事务失败: {}", e)).into_response();
    }

    let details = format!("{} -> {}", payload.name, payload.new_name);
    let _ = log_audit(&pool, username, "rename", Some(&old_dir_path), Some(&details), None, None).await;
    (StatusCode::OK, "重命名成功").into_response()
}

/// 更新目录子树中所有子记录的 parent_path（事务内调用）
async fn update_child_parent_paths(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    username: &str,
    old_prefix: &str,
    new_prefix: &str,
) -> Result<(), sqlx::Error> {
    // 更新直接子节点：parent_path = old_prefix
    sqlx::query("UPDATE files SET parent_path = ? WHERE username = ? AND parent_path = ?")
        .bind(new_prefix)
        .bind(username)
        .bind(old_prefix)
        .execute(&mut **tx)
        .await?;

    // 更新深层子节点：parent_path LIKE old_prefix/%
    let like_pattern = format!("{}/%", old_prefix);
    sqlx::query(
        "UPDATE files SET parent_path = ? || SUBSTR(parent_path, ? + 1) WHERE username = ? AND parent_path LIKE ?"
    )
    .bind(new_prefix)
    .bind(old_prefix.len() as i64)
    .bind(username)
    .bind(&like_pattern)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// 文件系统回滚：将已移动的文件移回原位
fn rollback_moved_files(moved: &[(String, std::path::PathBuf, std::path::PathBuf)]) {
    for (_, src, dst) in moved.iter().rev() {
        let _ = std::fs::rename(dst, src);
    }
}

// 4. 移动单个项目（包括目录子树）
pub async fn move_item(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<MoveRequest>,
) -> impl IntoResponse {
    let username = &session.username;
    let src_parent = user_dir_path(payload.current_path);
    let target_parent = user_dir_path(Some(payload.target_dir));

    let base_path = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let src_p = safe_join_sandbox(base_path, &format!("{}/{}/{}", username, src_parent, payload.name));
    let dst_p = safe_join_sandbox(base_path, &format!("{}/{}/{}", username, target_parent, payload.name));

    if let Err(e) = tokio::fs::rename(&src_p, &dst_p).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("移动物理结构失败: {}", e)).into_response();
    }

    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            let _ = tokio::fs::rename(&dst_p, &src_p).await;
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("开启事务失败: {}", e)).into_response();
        }
    };

    // 更新被移动条目自身的 parent_path
    if let Err(e) = sqlx::query("UPDATE files SET parent_path = ? WHERE username = ? AND name = ? AND parent_path = ?")
        .bind(&target_parent)
        .bind(username)
        .bind(&payload.name)
        .bind(&src_parent)
        .execute(&mut *tx)
        .await
    {
        let _ = tokio::fs::rename(&dst_p, &src_p).await;
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("数据库更新失败: {}", e)).into_response();
    }

    // 如果是目录，更新所有子记录的 parent_path
    let old_dir_path = if src_parent.is_empty() { payload.name.clone() } else { format!("{}/{}", src_parent, payload.name) };
    let new_dir_path = if target_parent.is_empty() { payload.name.clone() } else { format!("{}/{}", target_parent, payload.name) };
    if let Err(e) = update_child_parent_paths(&mut tx, username, &old_dir_path, &new_dir_path).await {
        let _ = tokio::fs::rename(&dst_p, &src_p).await;
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("更新子目录记录失败: {}", e)).into_response();
    }

    if let Err(e) = tx.commit().await {
        let _ = tokio::fs::rename(&dst_p, &src_p).await;
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("提交事务失败: {}", e)).into_response();
    }

    let details = format!("{} -> {}", src_parent, target_parent);
    let _ = log_audit(&pool, username, "move", Some(&old_dir_path), Some(&details), None, None).await;
    (StatusCode::OK, "迁移路径成功").into_response()
}

// 5. 批量移动（事务内原子执行）
pub async fn move_batch(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<MoveBatchRequest>,
) -> impl IntoResponse {
    let username = &session.username;
    let src_parent = user_dir_path(payload.current_path);
    let target_parent = user_dir_path(Some(payload.target_path));
    let base_path = std::path::Path::new(crate::constants::UPLOADS_DIR);

    // 先执行所有文件系统操作（记录成功项以便回滚）
    let mut moved: Vec<(String, std::path::PathBuf, std::path::PathBuf)> = Vec::new();
    for name in &payload.names {
        let src_p = safe_join_sandbox(base_path, &format!("{}/{}/{}", username, src_parent, name));
        let dst_p = safe_join_sandbox(base_path, &format!("{}/{}/{}", username, target_parent, name));

        if let Err(e) = tokio::fs::rename(&src_p, &dst_p).await {
            // 回滚已移动的项目
            for (_, src, dst) in moved.iter().rev() {
                let _ = tokio::fs::rename(dst, src).await;
            }
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("移动项目 '{}' 失败: {}", name, e)).into_response();
        }
        moved.push((name.clone(), src_p, dst_p));
    }

    // 在事务内更新数据库
    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            rollback_moved_files(&moved);
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("开启事务失败: {}", e)).into_response();
        }
    };

    for (name, _, _) in &moved {
        if let Err(e) = sqlx::query(
            "UPDATE files SET parent_path = ? WHERE username = ? AND name = ? AND parent_path = ?"
        )
        .bind(&target_parent)
        .bind(username)
        .bind(name)
        .bind(&src_parent)
        .execute(&mut *tx)
        .await
        {
            rollback_moved_files(&moved);
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("数据库更新失败: {}", e)).into_response();
        }

        // 如果是目录，更新子树 parent_path
        let old_dir = if src_parent.is_empty() { name.clone() } else { format!("{}/{}", src_parent, name) };
        let new_dir = if target_parent.is_empty() { name.clone() } else { format!("{}/{}", target_parent, name) };
        if let Err(e) = update_child_parent_paths(&mut tx, username, &old_dir, &new_dir).await {
            rollback_moved_files(&moved);
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("更新子目录记录失败: {}", e)).into_response();
        }
    }

    if let Err(e) = tx.commit().await {
        rollback_moved_files(&moved);
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("提交事务失败: {}", e)).into_response();
    }

    let target = format!("[{}]", payload.names.join(", "));
    let details = format!("{} -> {}", src_parent, target_parent);
    let _ = log_audit(&pool, username, "move_batch", Some(&target), Some(&details), None, None).await;
    (StatusCode::OK, "批量移动成功").into_response()
}

// 6. 删除项目（移至回收站）
pub async fn delete_item(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<DeleteRequest>,
) -> impl IntoResponse {
    use uuid::Uuid;
    let username = &session.username;
    let parent_path = user_dir_path(payload.current_path);
    let full_logical_path = if parent_path.is_empty() { payload.name.clone() } else { format!("{}/{}", parent_path, payload.name) };

    let base_path = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let physical_src = safe_join_sandbox(base_path, &format!("{}/{}", username, full_logical_path));
    if !physical_src.exists() { return (StatusCode::NOT_FOUND, "目标实体不存在").into_response(); }

    let trash_uuid = Uuid::new_v4().to_string();
    let trash_dir = base_path.join("tmp").join("trash");
    let _ = tokio::fs::create_dir_all(&trash_dir).await;
    let physical_dst = trash_dir.join(&trash_uuid);

    if let Err(e) = tokio::fs::rename(&physical_src, &physical_dst).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("移动到物理回收站失败: {}", e)).into_response();
    }

    let _ = sqlx::query("INSERT INTO trash (username, original_path, trash_uuid) VALUES (?, ?, ?)")
        .bind(username).bind(&full_logical_path).bind(&trash_uuid).execute(&pool).await;
    // 删除被删除条目本身
    let _ = sqlx::query("DELETE FROM files WHERE username = ? AND name = ? AND parent_path = ?")
        .bind(username).bind(&payload.name).bind(&parent_path).execute(&pool).await;
    // 如果是目录，级联删除子文件/子目录记录
    let child_prefix = if parent_path.is_empty() {
        format!("{}/%", payload.name)
    } else {
        format!("{}/{}/%", parent_path, payload.name)
    };
    let _ = sqlx::query("DELETE FROM files WHERE username = ? AND parent_path LIKE ?")
        .bind(username).bind(&child_prefix).execute(&pool).await;

    let _ = update_user_used_mb(&pool, username).await;
    let _ = log_audit(&pool, username, "delete", Some(&full_logical_path), None, None, None).await;
    (StatusCode::OK, "已成功移至回收站").into_response()
}