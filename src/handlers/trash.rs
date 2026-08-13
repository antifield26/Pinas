use axum::{extract::Extension, http::StatusCode, response::Json};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Row};

use crate::core::UserSession;
use crate::error::{AppError, AppResult};
use crate::handlers::utils::{log_audit, safe_join_sandbox, update_user_used_mb};

#[derive(Serialize, FromRow)]
pub struct TrashItem {
    pub id: i64,
    pub original_path: String,
    pub deleted_at: String,
}

#[derive(Deserialize)]
pub struct TrashActionRequest {
    pub id: i64,
}

// 列出回收站（只读，不记录审计日志）
#[tracing::instrument(skip_all)]
pub async fn list_trash(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> AppResult<Json<Vec<TrashItem>>> {
    let rows = sqlx::query_as::<_, TrashItem>(
        "SELECT id, original_path, deleted_at FROM trash WHERE username = ?",
    )
    .bind(&session.username)
    .fetch_all(&pool)
    .await?;
    Ok(Json(rows))
}

// 从回收站还原
#[tracing::instrument(skip_all)]
pub async fn restore_trash(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<TrashActionRequest>,
) -> AppResult<(StatusCode, &'static str)> {
    let id = payload.id;
    let username = &session.username;

    let row =
        sqlx::query("SELECT original_path, trash_uuid FROM trash WHERE id = ? AND username = ?")
            .bind(id)
            .bind(username)
            .fetch_optional(&pool)
            .await?
            .ok_or_else(|| AppError::not_found("未查询到回收记录"))?;

    let orig_path: String = row.get("original_path");
    let trash_uuid: String = row.get("trash_uuid");

    let base_path = std::path::Path::new(crate::constants::UPLOADS_DIR);
    let trash_dir = std::path::Path::new(crate::constants::TRASH_DIR);
    let src = trash_dir.join(&trash_uuid);
    let dst = safe_join_sandbox(base_path, &format!("{}/{}", username, orig_path))?;

    if dst.exists() {
        return Err(AppError::conflict("目标路径已存在，请先移动或删除同名文件"));
    }

    // 配额预检：恢复大文件同样计入配额，超限拒绝（此前恢复可无限制撑爆配额）
    let src_size: u64 = dir_size(&src).await;
    let (current_used, quota): (i64, i64) =
        sqlx::query_as("SELECT used_mb, quota_mb FROM users WHERE username = ?")
            .bind(username)
            .fetch_optional(&pool)
            .await
            .map_err(|e| AppError::internal_log("查询用户配额", e))?
            .ok_or_else(|| AppError::not_found("用户不存在"))?;
    if current_used + crate::handlers::utils::bytes_to_mb_ceil(src_size) > quota {
        return Err(AppError::forbidden(format!(
            "存储空间不足，配额 {} MB，已使用 {} MB",
            quota, current_used
        )));
    }

    if let Some(p) = dst.parent() {
        let _ = tokio::fs::create_dir_all(p).await;
    }
    tokio::fs::rename(&src, &dst)
        .await
        .map_err(|e| AppError::internal_log("回收站还原", e))?;

    let path_obj = std::path::Path::new(&orig_path);
    let name = path_obj
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let parent = path_obj
        .parent()
        .unwrap_or(std::path::Path::new(""))
        .to_string_lossy()
        .to_string();
    let parent_cleaned = if parent == "/" {
        "".to_string()
    } else {
        parent
    };

    if dst.is_dir() {
        let _ = restore_dir_recursive(&pool, username, &dst, &parent_cleaned).await;
    } else {
        let meta = dst.metadata().map(|m| m.len()).unwrap_or(0);
        let size_mb = meta as f64 / crate::handlers::utils::BYTES_PER_MB_F64;
        let _ = sqlx::query("INSERT INTO files (username, name, parent_path, is_dir, size_mb) VALUES (?, ?, ?, 0, ?)")
            .bind(username).bind(&name).bind(&parent_cleaned).bind(size_mb).execute(&pool).await;
    }

    let _ = sqlx::query("DELETE FROM trash WHERE id = ?")
        .bind(id)
        .execute(&pool)
        .await;
    let _ = update_user_used_mb(&pool, username).await;
    let _ = log_audit(
        &pool,
        username,
        "restore",
        Some(&orig_path),
        None,
        None,
        None,
    )
    .await;
    Ok((StatusCode::OK, "目标已恢复原位"))
}

/// 计算路径总字节数（文件直接用大小；目录迭代式累加——async fn 递归需装箱，迭代更直白）
async fn dir_size(path: &std::path::Path) -> u64 {
    let mut total: u64 = 0;
    let mut stack: Vec<std::path::PathBuf> = vec![path.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let meta = match tokio::fs::metadata(&cur).await {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_file() {
            total += meta.len();
            continue;
        }
        if let Ok(mut entries) = tokio::fs::read_dir(&cur).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                stack.push(entry.path());
            }
        }
    }
    total
}

// 递归恢复目录辅助函数
async fn restore_dir_recursive(
    pool: &sqlx::SqlitePool,
    username: &str,
    dir_path: &std::path::Path,
    parent_path: &str,
) -> Result<(), String> {
    let dir_name = dir_path
        .file_name()
        .ok_or_else(|| format!("无法获取目录名: {:?}", dir_path))?
        .to_string_lossy()
        .into_owned();
    let _ =
        sqlx::query("INSERT INTO files (username, name, parent_path, is_dir) VALUES (?, ?, ?, 1)")
            .bind(username)
            .bind(&dir_name)
            .bind(parent_path)
            .execute(pool)
            .await;

    let new_parent = if parent_path.is_empty() {
        dir_name.clone()
    } else {
        format!("{}/{}", parent_path, dir_name)
    };
    if let Ok(mut entries) = tokio::fs::read_dir(dir_path).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let p = entry.path();
            let n = entry.file_name().to_string_lossy().into_owned();
            if p.is_dir() {
                Box::pin(restore_dir_recursive(pool, username, &p, &new_parent)).await?;
            } else {
                let m = p.metadata().map(|m| m.len()).unwrap_or(0);
                let size_mb = m as f64 / crate::handlers::utils::BYTES_PER_MB_F64;
                let _ = sqlx::query("INSERT INTO files (username, name, parent_path, is_dir, size_mb) VALUES (?, ?, ?, 0, ?)")
                    .bind(username)
                    .bind(&n)
                    .bind(&new_parent)
                    .bind(size_mb)
                    .execute(pool)
                    .await;
            }
        }
    }
    Ok(())
}

// 永久删除回收站中的单个项目
#[tracing::instrument(skip_all)]
pub async fn delete_trash_permanent(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<TrashActionRequest>,
) -> AppResult<(StatusCode, &'static str)> {
    let row =
        sqlx::query("SELECT trash_uuid, original_path FROM trash WHERE id = ? AND username = ?")
            .bind(payload.id)
            .bind(&session.username)
            .fetch_optional(&pool)
            .await?
            .ok_or_else(|| AppError::not_found("未匹配到相关项"))?;

    let uuid: String = row.get("trash_uuid");
    let original_path: String = row.get("original_path");
    let p = std::path::Path::new(crate::constants::TRASH_DIR).join(&uuid);
    if p.is_dir() {
        let _ = tokio::fs::remove_dir_all(&p).await;
    } else {
        let _ = tokio::fs::remove_file(&p).await;
    }

    let _ = sqlx::query("DELETE FROM trash WHERE id = ?")
        .bind(payload.id)
        .execute(&pool)
        .await;
    let _ = update_user_used_mb(&pool, &session.username).await;
    let _ = log_audit(
        &pool,
        &session.username,
        "permanent_delete",
        Some(&original_path),
        None,
        None,
        None,
    )
    .await;
    Ok((StatusCode::OK, "已从磁盘彻底碎纸抹除"))
}

/// 物理删除某用户回收站的全部条目（文件 + DB 行），返回条目数。
/// JSON clear_trash 与 HTMX trash_clear_fragment 共用，避免两路径行为漂移。
async fn clear_trash_physical(pool: &sqlx::SqlitePool, username: &str) -> usize {
    let rows = sqlx::query("SELECT id, trash_uuid FROM trash WHERE username = ?")
        .bind(username)
        .fetch_all(pool)
        .await
        .unwrap_or_default();

    let count = rows.len();
    for row in rows {
        let id: i64 = row.get("id");
        let trash_uuid: String = row.get("trash_uuid");
        let p = std::path::Path::new(crate::constants::TRASH_DIR).join(&trash_uuid);
        if p.is_dir() {
            let _ = tokio::fs::remove_dir_all(&p).await;
        } else {
            let _ = tokio::fs::remove_file(&p).await;
        }
        let _ = sqlx::query("DELETE FROM trash WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await;
    }
    count
}

// 清空回收站（当前用户所有项目）
#[tracing::instrument(skip_all)]
pub async fn clear_trash(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> AppResult<(StatusCode, &'static str)> {
    let count = clear_trash_physical(&pool, &session.username).await;

    let _ = update_user_used_mb(&pool, &session.username).await;
    let _ = log_audit(
        &pool,
        &session.username,
        "clear_trash",
        None,
        Some(&format!("{} items", count)),
        None,
        None,
    )
    .await;
    Ok((StatusCode::OK, "回收站已清空"))
}

// 回收站自动清理（超过30天自动删除） – 此函数由后台任务调用，不需要审计日志
pub async fn clean_expired_trash(pool: &sqlx::SqlitePool, days: u32) -> Result<(), sqlx::Error> {
    use chrono::{Duration, Utc};
    let cutoff = Utc::now() - Duration::days(days as i64);
    let cutoff_str = cutoff.format("%Y-%m-%d %H:%M:%S").to_string();

    let rows = sqlx::query("SELECT id, trash_uuid FROM trash WHERE deleted_at <= ?")
        .bind(&cutoff_str)
        .fetch_all(pool)
        .await?;

    for row in rows {
        let id: i64 = row.get("id");
        let trash_uuid: String = row.get("trash_uuid");
        let physical_path = std::path::Path::new(crate::constants::TRASH_DIR).join(&trash_uuid);

        if physical_path.is_dir() {
            let _ = tokio::fs::remove_dir_all(&physical_path).await;
        } else {
            let _ = tokio::fs::remove_file(&physical_path).await;
        }

        let _ = sqlx::query("DELETE FROM trash WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await;
    }

    Ok(())
}

// ====== HTMX Fragment 处理器 ======
use crate::templates::AppTemplate;
use askama::Template;

#[derive(Template)]
#[template(path = "components/trash_list.html")]
struct TrashListFragment {
    items: Vec<TrashRowData>,
}

struct TrashRowData {
    id: i64,
    original_path: String,
    deleted_at: String,
}

#[tracing::instrument(skip_all)]
pub async fn trash_list_fragment(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> impl axum::response::IntoResponse {
    let rows = sqlx::query_as::<_, (i64, String, String)>(
        "SELECT id, original_path, deleted_at FROM trash WHERE username = ? ORDER BY deleted_at DESC"
    )
    .bind(&session.username)
    .fetch_all(&pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(id, original_path, deleted_at)| TrashRowData { id, original_path, deleted_at })
    .collect::<Vec<_>>();

    AppTemplate(TrashListFragment { items: rows })
}

#[tracing::instrument(skip_all)]
pub async fn trash_clear_fragment(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> impl axum::response::IntoResponse {
    // 与 JSON clear_trash 共用物理删除逻辑：只删 DB 行会留下孤儿文件持续占盘
    let count = clear_trash_physical(&pool, &session.username).await;

    let _ = log_audit(
        &pool,
        &session.username,
        "trash_clear",
        None,
        Some(&format!("{} items", count)),
        None,
        None,
    )
    .await;

    AppTemplate(TrashListFragment { items: vec![] })
}
