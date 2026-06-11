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

// 1. 获取文件列表（支持搜索和排序）
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

    let mut sql = String::from("SELECT id, name, parent_path, is_dir, size_mb, created_at FROM files WHERE username = ?");
    
    if has_search {
        if current_path.is_empty() {
            sql.push_str(" AND name LIKE ?");
        } else {
            sql.push_str(" AND (parent_path = ? OR parent_path LIKE ?) AND name LIKE ?");
        }
    } else {
        sql.push_str(" AND parent_path = ?");
    }

    if let Some(ref sort) = query.sort_by {
        match sort.as_str() {
            "name" => sql.push_str(" ORDER BY is_dir DESC, name ASC"),
            "size" => sql.push_str(" ORDER BY is_dir DESC, CAST(size_mb AS REAL) DESC"),
            "time" => sql.push_str(" ORDER BY is_dir DESC, created_at DESC"),
            _ => sql.push_str(" ORDER BY is_dir DESC, name ASC"),
        }
    } else {
        sql.push_str(" ORDER BY is_dir DESC, name ASC");
    }

    let mut db_query = sqlx::query_as::<_, FileItem>(&sql).bind(&username);

    let like_pattern_opt = if has_search && !current_path.is_empty() {
        Some(format!("{}/%", current_path))
    } else {
        None
    };

    if has_search {
        if current_path.is_empty() {
            db_query = db_query.bind(&search_pattern);
        } else {
            let like_pattern = like_pattern_opt.as_ref().unwrap();
            db_query = db_query
                .bind(&current_path)
                .bind(like_pattern)
                .bind(&search_pattern);
        }
    } else {
        db_query = db_query.bind(&current_path);
    }

    match db_query.fetch_all(&pool).await {
        Ok(files) => (StatusCode::OK, Json(files)).into_response(),
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
    let base_path = std::path::Path::new("uploads");
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
            let _ = log_audit(&pool, username, "create_folder", Some(&target), None).await;
            (StatusCode::OK, "文件夹创建成功").into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("数据库录入失败: {}", e)).into_response(),
    }
}

// 3. 重命名
pub async fn rename_item(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<RenameRequest>,
) -> impl IntoResponse {
    let username = &session.username;
    let parent_path = user_dir_path(payload.current_path);
    let base_path = std::path::Path::new("uploads");

    let old_p = safe_join_sandbox(base_path, &format!("{}/{}/{}", username, parent_path, payload.name));
    let new_p = safe_join_sandbox(base_path, &format!("{}/{}/{}", username, parent_path, payload.new_name));

    if let Err(e) = tokio::fs::rename(&old_p, &new_p).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("重命名物理媒介失败: {}", e)).into_response();
    }

    let _ = sqlx::query("UPDATE files SET name = ? WHERE username = ? AND name = ? AND parent_path = ?")
        .bind(&payload.new_name)
        .bind(username)
        .bind(&payload.name)
        .bind(&parent_path)
        .execute(&pool)
        .await;

    let target = if parent_path.is_empty() {
        payload.name.clone()
    } else {
        format!("{}/{}", parent_path, payload.name)
    };
    let details = format!("{} -> {}", payload.name, payload.new_name);
    let _ = log_audit(&pool, username, "rename", Some(&target), Some(&details)).await;
    (StatusCode::OK, "重命名成功").into_response()
}

// 4. 移动单个项目
pub async fn move_item(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<MoveRequest>,
) -> impl IntoResponse {
    let username = &session.username;
    let src_parent = user_dir_path(payload.current_path);
    let target_parent = user_dir_path(Some(payload.target_dir));

    let base_path = std::path::Path::new("uploads");
    let src_p = safe_join_sandbox(base_path, &format!("{}/{}/{}", username, src_parent, payload.name));
    let dst_p = safe_join_sandbox(base_path, &format!("{}/{}/{}", username, target_parent, payload.name));

    if let Err(e) = tokio::fs::rename(&src_p, &dst_p).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("移动物理结构失败: {}", e)).into_response();
    }

    let _ = sqlx::query("UPDATE files SET parent_path = ? WHERE username = ? AND name = ? AND parent_path = ?")
        .bind(&target_parent)
        .bind(username)
        .bind(&payload.name)
        .bind(&src_parent)
        .execute(&pool)
        .await;

    let target = if src_parent.is_empty() {
        payload.name.clone()
    } else {
        format!("{}/{}", src_parent, payload.name)
    };
    let details = format!("{} -> {}", src_parent, target_parent);
    let _ = log_audit(&pool, username, "move", Some(&target), Some(&details)).await;
    (StatusCode::OK, "迁移路径成功").into_response()
}

// 5. 批量移动
pub async fn move_batch(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<MoveBatchRequest>,
) -> impl IntoResponse {
    let username = &session.username;
    let src_parent = user_dir_path(payload.current_path);
    let target_parent = user_dir_path(Some(payload.target_path));
    let base_path = std::path::Path::new("uploads");

    for name in &payload.names {
        let src_p = safe_join_sandbox(base_path, &format!("{}/{}/{}", username, src_parent, name));
        let dst_p = safe_join_sandbox(base_path, &format!("{}/{}/{}", username, target_parent, name));

        if let Err(e) = tokio::fs::rename(&src_p, &dst_p).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("移动项目 '{}' 失败: {}", name, e)).into_response();
        }

        let result = sqlx::query(
            "UPDATE files SET parent_path = ? WHERE username = ? AND name = ? AND parent_path = ?"
        )
        .bind(&target_parent)
        .bind(username)
        .bind(name)
        .bind(&src_parent)
        .execute(&pool)
        .await;

        if let Err(e) = result {
            let _ = tokio::fs::rename(&dst_p, &src_p).await;
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("数据库更新失败: {}", e)).into_response();
        }
    }

    let target = format!("[{}]", payload.names.join(", "));
    let details = format!("{} -> {}", src_parent, target_parent);
    let _ = log_audit(&pool, username, "move_batch", Some(&target), Some(&details)).await;
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

    let base_path = std::path::Path::new("uploads");
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
    let _ = sqlx::query("DELETE FROM files WHERE username = ? AND name = ? AND parent_path = ?")
        .bind(username).bind(&payload.name).bind(&parent_path).execute(&pool).await;

    let _ = update_user_used_mb(&pool, username).await;
    let _ = log_audit(&pool, username, "delete", Some(&full_logical_path), None).await;
    (StatusCode::OK, "已成功移至回收站").into_response()
}