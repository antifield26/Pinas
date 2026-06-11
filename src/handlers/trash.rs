use axum::{
    extract::Extension,
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::Serialize;
use sqlx::{FromRow, Row};

use crate::handlers::utils::{safe_join_sandbox, update_user_used_mb, log_audit};
use pinas_core::UserSession;

#[derive(Serialize, FromRow)]
pub struct TrashItem {
    pub id: i64,
    pub original_path: String,
    pub deleted_at: String,
}

// 列出回收站（只读，不记录审计日志）
pub async fn list_trash(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> impl IntoResponse {
    let rows = sqlx::query_as::<_, TrashItem>("SELECT id, original_path, deleted_at FROM trash WHERE username = ?")
        .bind(&session.username)
        .fetch_all(&pool)
        .await
        .unwrap_or_default();
    Json(rows)
}

// 从回收站还原
pub async fn restore_trash(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    let id = payload.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
    let username = &session.username;

    let trash_meta = sqlx::query("SELECT original_path, trash_uuid FROM trash WHERE id = ? AND username = ?")
        .bind(id)
        .bind(username)
        .fetch_optional(&pool)
        .await
        .unwrap_or(None);

    if let Some(row) = trash_meta {
        let orig_path: String = row.get("original_path");
        let trash_uuid: String = row.get("trash_uuid");

        let base_path = std::path::Path::new("uploads");
        let src = base_path.join("tmp").join("trash").join(&trash_uuid);
        let dst = safe_join_sandbox(base_path, &format!("{}/{}", username, orig_path));

        // 检查目标是否已存在
        if dst.exists() {
            return (StatusCode::CONFLICT, format!("目标路径已存在: {}", dst.display())).into_response();
        }

        if let Some(p) = dst.parent() {
            let _ = tokio::fs::create_dir_all(p).await;
        }
        if let Err(e) = tokio::fs::rename(&src, &dst).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("物理媒介归位失败: {}", e)).into_response();
        }

        let path_obj = std::path::Path::new(&orig_path);
        let name = path_obj.file_name().unwrap_or_default().to_string_lossy().to_string();
        let parent = path_obj.parent().unwrap_or(std::path::Path::new("")).to_string_lossy().to_string();
        let parent_cleaned = if parent == "/" { "".to_string() } else { parent };

        if dst.is_dir() {
            let _ = restore_dir_recursive(&pool, username, &dst, &parent_cleaned).await;
        } else {
            let meta = dst.metadata().map(|m| m.len()).unwrap_or(0);
            let size_mb = format!("{:.2}", meta as f64 / 1048576.0);
            let _ = sqlx::query("INSERT INTO files (username, name, parent_path, is_dir, size_mb) VALUES (?, ?, ?, 0, ?)")
                .bind(username)
                .bind(&name)
                .bind(&parent_cleaned)
                .bind(&size_mb)
                .execute(&pool)
                .await;
        }

        let _ = sqlx::query("DELETE FROM trash WHERE id = ?").bind(id).execute(&pool).await;
        if let Err(e) = update_user_used_mb(&pool, username).await {
            tracing::error!("恢复文件后更新配额失败: {}", e);
        }

        // 审计日志：还原文件
        let _ = log_audit(&pool, username, "restore", Some(&orig_path), None).await;
        return (StatusCode::OK, "目标已恢复原位").into_response();
    }

    (StatusCode::NOT_FOUND, "未查询到回收记录").into_response()
}

// 递归恢复目录辅助函数
async fn restore_dir_recursive(
    pool: &sqlx::SqlitePool,
    username: &str,
    dir_path: &std::path::Path,
    parent_path: &str,
) -> Result<(), String> {
    let dir_name = dir_path.file_name().unwrap().to_string_lossy().into_owned();
    let _ = sqlx::query("INSERT INTO files (username, name, parent_path, is_dir, size_mb) VALUES (?, ?, ?, 1, '-')")
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
                let size_mb = format!("{:.2}", m as f64 / 1048576.0);
                let _ = sqlx::query("INSERT INTO files (username, name, parent_path, is_dir, size_mb) VALUES (?, ?, ?, 0, ?)")
                    .bind(username)
                    .bind(&n)
                    .bind(&new_parent)
                    .bind(&size_mb)
                    .execute(pool)
                    .await;
            }
        }
    }
    Ok(())
}

// 永久删除回收站中的单个项目
pub async fn delete_trash_permanent(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    let id = payload.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
    let username = &session.username;

    if let Ok(Some(row)) = sqlx::query("SELECT trash_uuid, original_path FROM trash WHERE id = ? AND username = ?")
        .bind(id)
        .bind(username)
        .fetch_optional(&pool)
        .await
    {
        let uuid: String = row.get("trash_uuid");
        let original_path: String = row.get("original_path");
        let p = std::path::Path::new("uploads")
            .join("tmp")
            .join("trash")
            .join(&uuid);
        if p.is_dir() {
            let _ = tokio::fs::remove_dir_all(&p).await;
        } else {
            let _ = tokio::fs::remove_file(&p).await;
        }
        let _ = sqlx::query("DELETE FROM trash WHERE id = ?").bind(id).execute(&pool).await;
        if let Err(e) = update_user_used_mb(&pool, username).await {
            tracing::error!("永久删除后更新配额失败: {}", e);
        }

        // 审计日志：永久删除
        let _ = log_audit(&pool, username, "permanent_delete", Some(&original_path), None).await;
        return (StatusCode::OK, "已从磁盘彻底碎纸抹除").into_response();
    }
    (StatusCode::NOT_FOUND, "未匹配到相关项").into_response()
}

// 清空回收站（当前用户所有项目）
pub async fn clear_trash(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> impl IntoResponse {
    let username = &session.username;

    let rows = sqlx::query("SELECT id, trash_uuid, original_path FROM trash WHERE username = ?")
        .bind(username)
        .fetch_all(&pool)
        .await
        .unwrap_or_default();

    let count = rows.len();
    for row in rows {
        let id: i64 = row.get("id");
        let trash_uuid: String = row.get("trash_uuid");
        let physical_path = std::path::Path::new("uploads")
            .join("tmp")
            .join("trash")
            .join(&trash_uuid);

        if physical_path.is_dir() {
            let _ = tokio::fs::remove_dir_all(&physical_path).await;
        } else {
            let _ = tokio::fs::remove_file(&physical_path).await;
        }

        let _ = sqlx::query("DELETE FROM trash WHERE id = ?").bind(id).execute(&pool).await;
    }

    if let Err(e) = update_user_used_mb(&pool, username).await {
        tracing::error!("clear_trash: 更新用户容量失败 (用户: {}): {}", username, e);
    }

    // 审计日志：清空回收站
    let details = format!("{} items", count);
    let _ = log_audit(&pool, username, "clear_trash", None, Some(&details)).await;
    (StatusCode::OK, "回收站已清空").into_response()
}

// 回收站自动清理（超过30天自动删除） – 此函数由后台任务调用，不需要审计日志
pub async fn clean_expired_trash(pool: &sqlx::SqlitePool, days: u32) -> Result<(), sqlx::Error> {
    use chrono::{Utc, Duration};
    let cutoff = Utc::now() - Duration::days(days as i64);
    let cutoff_str = cutoff.format("%Y-%m-%d %H:%M:%S").to_string();

    let rows = sqlx::query(
        "SELECT id, trash_uuid FROM trash WHERE deleted_at <= ?"
    )
    .bind(&cutoff_str)
    .fetch_all(pool)
    .await?;

    for row in rows {
        let id: i64 = row.get("id");
        let trash_uuid: String = row.get("trash_uuid");
        let physical_path = std::path::Path::new("uploads")
            .join("tmp")
            .join("trash")
            .join(&trash_uuid);

        if physical_path.is_dir() {
            let _ = tokio::fs::remove_dir_all(&physical_path).await;
        } else {
            let _ = tokio::fs::remove_file(&physical_path).await;
        }

        let _ = sqlx::query("DELETE FROM trash WHERE id = ?").bind(id).execute(pool).await;
    }

    Ok(())
}