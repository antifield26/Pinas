// ====== 文件操作意图日志（write-ahead journal） ======
// 修复 FS 与 DB 两步非原子的崩溃窗口（M1）：rename/move/delete 在物理操作前先落一条
// 意图记录（fs_journal 表），操作完整完成后删除。进程崩溃重启时重放：
//   - FS 已完成而 DB 未动 → 补齐 DB（幂等）
//   - FS 未动 → 重做物理操作 + DB
//   - 双方均缺失 → 告警并放弃该记录
// 另含 WebDAV MOVE 覆盖的位移目标（uploads/.dav_disp）启动恢复（M9）。

use crate::constants::{DAV_DISP_DIR, UPLOADS_DIR};
use crate::error::{AppError, AppResult};
use sqlx::SqlitePool;
use tracing::{info, warn};

/// 插入意图记录，返回记录 id（完成后必须 remove）
pub async fn insert(
    pool: &SqlitePool,
    username: &str,
    op: &str,
    src: &str,
    dst: &str,
) -> AppResult<i64> {
    let id = sqlx::query("INSERT INTO fs_journal (username, op, src, dst) VALUES (?, ?, ?, ?)")
        .bind(username)
        .bind(op)
        .bind(src)
        .bind(dst)
        .execute(pool)
        .await
        .map_err(|e| AppError::internal_log("写入意图日志", e))?
        .last_insert_rowid();
    Ok(id)
}

/// 删除意图记录（操作完整收尾）
pub async fn remove(pool: &SqlitePool, id: i64) {
    let _ = sqlx::query("DELETE FROM fs_journal WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await;
}

/// 批量删除（move_batch 等场景）
pub async fn remove_many(pool: &SqlitePool, ids: &[i64]) {
    for id in ids {
        remove(pool, *id).await;
    }
}

fn split_last(path: &str) -> (String, String) {
    match path.rsplit_once('/') {
        Some((p, n)) => (p.to_string(), n.to_string()),
        None => (String::new(), path.to_string()),
    }
}

/// rename/move 的 DB 部分（幂等，可被重放安全重复执行）。
/// op ∈ {"rename", "move"}；src/dst 为逻辑路径（不含用户名，空串=根）。
pub async fn apply_db_rename_move(
    pool: &SqlitePool,
    username: &str,
    op: &str,
    src: &str,
    dst: &str,
) -> AppResult<()> {
    let (src_parent, src_name) = split_last(src);
    let (dst_parent, dst_name) = split_last(dst);
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| AppError::internal_log("开启重放事务", e))?;
    if op == "rename" {
        sqlx::query(
            "UPDATE files SET name = ? WHERE username = ? AND name = ? AND parent_path = ?",
        )
        .bind(&dst_name)
        .bind(username)
        .bind(&src_name)
        .bind(&src_parent)
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query(
            "UPDATE files SET parent_path = ? WHERE username = ? AND name = ? AND parent_path = ?",
        )
        .bind(&dst_parent)
        .bind(username)
        .bind(&src_name)
        .bind(&src_parent)
        .execute(&mut *tx)
        .await?;
    }
    crate::db::queries::update_child_paths(&mut tx, username, src, dst).await?;
    tx.commit()
        .await
        .map_err(|e| AppError::internal_log("提交重放事务", e))?;
    Ok(())
}

async fn replay_rename_move(
    pool: &SqlitePool,
    username: &str,
    op: &str,
    src: &str,
    dst: &str,
) -> AppResult<()> {
    // P0-4：重放物理操作同样经 openat2 沙箱（崩溃恢复场景不得成为越界通道）
    let sb = crate::fsutil::Sandbox::new(UPLOADS_DIR)
        .map_err(|e| AppError::internal_log("打开沙箱", e))?;
    let src_rel = format!("{username}/{src}");
    let dst_rel = format!("{username}/{dst}");
    let dst_exists = sb.try_exists(&dst_rel).unwrap_or(false);
    let src_exists = sb.try_exists(&src_rel).unwrap_or(false);
    if dst_exists {
        // FS 已完成：只补 DB
        apply_db_rename_move(pool, username, op, src, dst).await?;
    } else if src_exists {
        // FS 未完成：重做物理移动 + DB
        sb.rename(&src_rel, &dst_rel)
            .map_err(|e| AppError::internal_log("重放物理移动", e))?;
        apply_db_rename_move(pool, username, op, src, dst).await?;
    } else {
        warn!(
            "[journal] {}: src 与 dst 均不存在，放弃重放（数据可能已丢失）: {} -> {}",
            op, src, dst
        );
    }
    Ok(())
}

async fn replay_trash(pool: &SqlitePool, username: &str, src: &str, uuid: &str) -> AppResult<()> {
    let sb = crate::fsutil::Sandbox::new(UPLOADS_DIR)
        .map_err(|e| AppError::internal_log("打开沙箱", e))?;
    let src_rel = format!("{username}/{src}");
    // TRASH_DIR = uploads/.trash → 相对 uploads 根的 rel 恒为 .trash/{uuid}
    let trash_rel = format!(".trash/{uuid}");
    let row_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM trash WHERE trash_uuid = ?)")
            .bind(uuid)
            .fetch_one(pool)
            .await
            .unwrap_or(false);

    if sb.try_exists(&trash_rel).unwrap_or(false) {
        // FS 已完成：补 DB（trash 行 + 删除 files 行）
        if !row_exists {
            let _ = sqlx::query(
                "INSERT INTO trash (username, original_path, trash_uuid) VALUES (?, ?, ?)",
            )
            .bind(username)
            .bind(src)
            .bind(uuid)
            .execute(pool)
            .await;
        }
        let (parent, name) = split_last(src);
        crate::handlers::file_ops::db_delete_file_rows(pool, username, &parent, &name).await?;
    } else if !row_exists {
        // DB 尚未登记：FS 未完成（或 src 已被外部处理）
        if sb.try_exists(&src_rel).unwrap_or(false) {
            let _ = sb.create_dir_all(".trash");
            sb.rename(&src_rel, &trash_rel)
                .map_err(|e| AppError::internal_log("重放回收移动", e))?;
            let _ = sqlx::query(
                "INSERT INTO trash (username, original_path, trash_uuid) VALUES (?, ?, ?)",
            )
            .bind(username)
            .bind(src)
            .bind(uuid)
            .execute(pool)
            .await;
            let (parent, name) = split_last(src);
            crate::handlers::file_ops::db_delete_file_rows(pool, username, &parent, &name).await?;
        } else {
            warn!(
                "[journal] trash: src 与回收站文件均不存在，放弃重放: {}",
                src
            );
        }
    } else {
        // trash 行在但物理文件没了（外部清理）：放弃
        warn!(
            "[journal] trash: 回收站文件缺失（可能被外部清理），放弃重放: {}",
            uuid
        );
    }
    Ok(())
}

/// WebDAV MOVE 覆盖位移目标恢复：扫描 uploads/.dav_disp/{uuid}.json 元数据。
/// - 目标路径已存在（覆盖已完成）→ 清理位移文件
/// - 目标缺失（崩溃于覆盖中途）→ 还原位移文件 + 重建 DB 行
pub async fn recover_dav_disp(pool: &SqlitePool) {
    let dir = std::path::Path::new(DAV_DISP_DIR);
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    // P0-4：位移还原经 openat2 沙箱（root = uploads，.dav_disp 与目标同 root）
    let sb = match crate::fsutil::Sandbox::new(UPLOADS_DIR) {
        Ok(s) => s,
        Err(_) => return,
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let meta_path = entry.path();
        let Some(name) = meta_path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(uuid) = name.strip_suffix(".json") else {
            continue;
        };
        let disp_rel = format!("{}/{}", DAV_DISP_DIR.trim_start_matches("uploads/"), uuid);
        let meta = match tokio::fs::read_to_string(&meta_path)
            .await
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        {
            Some(m) => m,
            None => {
                warn!("[journal] dav 位移元数据损坏，跳过: {}", uuid);
                continue;
            }
        };
        let username = meta["username"].as_str().unwrap_or_default().to_string();
        let parent = meta["parent"].as_str().unwrap_or_default().to_string();
        let fname = meta["name"].as_str().unwrap_or_default().to_string();
        if username.is_empty() || fname.is_empty() {
            warn!("[journal] dav 位移元数据缺字段，跳过: {}", uuid);
            continue;
        }
        let target_rel = format!("{username}/{parent}/{fname}");
        if sb.try_exists(&target_rel).unwrap_or(false) {
            // 覆盖已完成：位移文件可清理
            let _ = sb.remove_dir_all(&disp_rel);
            let _ = tokio::fs::remove_file(&meta_path).await;
            info!("[journal] 已清理 dav 位移残留（覆盖已完成）: {parent}/{fname}");
        } else if sb.rename(&disp_rel, &target_rel).is_ok() {
            if let Some(arr) = meta["rows"].as_array() {
                for r in arr {
                    let _ = sqlx::query(
                        "INSERT OR IGNORE INTO files (username, name, parent_path, is_dir, size_mb, identifier) VALUES (?, ?, ?, ?, ?, ?)",
                    )
                    .bind(&username)
                    .bind(r["name"].as_str().unwrap_or_default())
                    .bind(r["parent_path"].as_str().unwrap_or_default())
                    .bind(r["is_dir"].as_i64().unwrap_or(0))
                    .bind(r["size_mb"].as_f64().unwrap_or(0.0))
                    .bind(r["identifier"].as_str())
                    .execute(pool)
                    .await;
                }
            }
            let _ = tokio::fs::remove_file(&meta_path).await;
            info!("[journal] 已还原 dav 位移目标: {parent}/{fname}");
        } else {
            warn!("[journal] dav 位移还原失败，保留待下次重试: {parent}/{fname}");
        }
    }
}

/// 启动重放入口：先恢复 rename/move/trash 意图，再处理 dav 位移残留。
/// 单条失败保留记录待下次启动重试，不阻断服务启动。
pub async fn replay(pool: &SqlitePool) {
    let entries: Vec<(i64, String, String, String, String)> =
        sqlx::query_as("SELECT id, username, op, src, dst FROM fs_journal ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap_or_default();
    if !entries.is_empty() {
        info!("[journal] 启动重放 {} 条待恢复意图", entries.len());
    }
    for (id, username, op, src, dst) in entries {
        let result = match op.as_str() {
            "rename" | "move" => replay_rename_move(pool, &username, &op, &src, &dst).await,
            "trash" => replay_trash(pool, &username, &src, &dst).await,
            other => {
                warn!("[journal] 未知操作类型，跳过: {other}");
                Ok(())
            }
        };
        match result {
            Ok(()) => remove(pool, id).await,
            Err(e) => {
                tracing::error!(
                    "[journal] 重放失败（保留记录待下次重试） id={} op={} src={}: {}",
                    id,
                    op,
                    src,
                    e
                );
            }
        }
    }
    recover_dav_disp(pool).await;
}
