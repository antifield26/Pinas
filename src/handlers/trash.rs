use axum::{extract::Extension, http::StatusCode, response::Json};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Row};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::core::UserSession;
use crate::error::{AppError, AppResult};
use crate::handlers::utils::{log_audit, safe_join_sandbox, update_user_used_mb};

// ====== P2-14：回收站 per-uuid 互斥锁 ======
// 背景：后台 clean_expired_trash 按 deleted_at 删除 `.trash/{uuid}`，与用户
// restore_trash / delete_trash_permanent / clear_trash_physical 对同一 uuid 的
// rename/删除无共享锁，低概率竞态可能误删正在还原的文件。
// 方案：按 trash_uuid 分桶的异步互斥锁表。回收站操作低频，竞争极小，分桶锁比
// 全局单一 async Mutex 更细粒度且实现同样简单，避免不同条目互相阻塞。
// 容量上限：回收站条目数量级远小于 10_000，超限即整体清空锁表（会短暂丢失
// 分桶锁语义，但对低频回收站操作无实际影响），防止 uuid 无限增长撑爆内存。
const TRASH_LOCKS_MAX: usize = 10_000;

static TRASH_LOCKS: LazyLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 获取按 trash_uuid 分桶的互斥锁（还原/永久删除/清空与后台到期清理共用）。
/// 返回 owned guard，跨 await 持有，直到作用域结束才释放。
pub async fn trash_lock(uuid: &str) -> OwnedMutexGuard<()> {
    let mut map = TRASH_LOCKS.lock().await;
    if map.len() >= TRASH_LOCKS_MAX {
        map.clear();
    }
    let inner = map
        .entry(uuid.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone();
    drop(map);
    inner.lock_owned().await
}

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

    // P2-14：持有该 uuid 的互斥锁直到还原完成，防止后台到期清理误删正在还原的文件
    let _guard = trash_lock(&trash_uuid).await;

    // P0-4：全部物理操作经 openat2 沙箱（root = uploads，.trash 与用户目录同 root）
    let sb = crate::fsutil::Sandbox::new(crate::constants::UPLOADS_DIR)
        .map_err(|e| AppError::internal_log("打开沙箱", e))?;
    let src_rel = format!(".trash/{}", trash_uuid);
    let dst_rel = format!("{}/{}", username, orig_path);
    let _src = sb.join(&src_rel);
    let _dst = safe_join_sandbox(
        std::path::Path::new(crate::constants::UPLOADS_DIR),
        &dst_rel,
    )?;

    if sb.try_exists(&dst_rel).unwrap_or(false) {
        return Err(AppError::conflict("目标路径已存在，请先移动或删除同名文件"));
    }

    // 配额预检：恢复大文件同样计入配额，超限拒绝（此前恢复可无限制撑爆配额）
    let src_size: u64 = dir_size(&sb, &src_rel).await;

    if let Some((parent_rel, _)) = dst_rel.rsplit_once('/') {
        let _ = sb.create_dir_all(parent_rel);
    }
    sb.rename(&src_rel, &dst_rel)
        .map_err(|e| AppError::internal_log("回收站还原", e))?;

    // M3/M4 修复：配额预检 + DB 登记 + trash 行删除进同一写事务（写锁串行化并发，
    // 消除 TOCTOU；失败整体回滚并还原物理文件，不再事后全量重算与增量互相覆盖）
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| AppError::internal_log("开启还原事务", e))?;
    if let Err(e) = crate::handlers::utils::check_and_adjust_quota_tx(
        &mut tx,
        username,
        crate::handlers::utils::bytes_to_mb_ceil(src_size),
    )
    .await
    {
        drop(tx);
        let _ = sb.rename(&dst_rel, &src_rel); // 回滚物理还原
        return Err(e);
    }

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

    let db_result = if sb.metadata(&dst_rel).map(|m| m.is_dir()).unwrap_or(false) {
        restore_dir_recursive_tx(&mut tx, username, &sb, &dst_rel, &parent_cleaned).await
    } else {
        let meta = sb.metadata(&dst_rel).map(|m| m.len()).unwrap_or(0);
        let size_mb = meta as f64 / crate::handlers::utils::BYTES_PER_MB_F64;
        sqlx::query(
            "INSERT INTO files (username, name, parent_path, is_dir, size_mb) VALUES (?, ?, ?, 0, ?)",
        )
        .bind(username)
        .bind(&name)
        .bind(&parent_cleaned)
        .bind(size_mb)
        .execute(&mut *tx)
        .await
        .map(|_| ())
        .map_err(|e| format!("文件行恢复失败: {e}"))
    };
    if let Err(e) = db_result {
        drop(tx);
        let _ = sb.rename(&dst_rel, &src_rel);
        tracing::error!("[Trash] 还原登记失败: {}", e);
        return Err(AppError::internal("还原失败，请稍后重试"));
    }

    if let Err(e) = sqlx::query("DELETE FROM trash WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await
    {
        drop(tx);
        let _ = sb.rename(&dst_rel, &src_rel);
        tracing::error!("[Trash] 回收行删除失败: {}", e);
        return Err(AppError::internal("还原失败，请稍后重试"));
    }
    if let Err(e) = tx.commit().await {
        let _ = sb.rename(&dst_rel, &src_rel);
        tracing::error!("[Trash] 还原事务提交失败: {}", e);
        return Err(AppError::internal("还原失败，请稍后重试"));
    }

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

/// 计算路径总字节数（沙箱内迭代式遍历；文件直接用大小，目录累加）
async fn dir_size(sb: &crate::fsutil::Sandbox, rel: &str) -> u64 {
    let mut total: u64 = 0;
    let mut stack: Vec<String> = vec![rel.to_string()];
    while let Some(cur) = stack.pop() {
        let meta = match sb.metadata(&cur) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_file() {
            total += meta.len();
            continue;
        }
        if let Ok(entries) = sb.read_dir(&cur) {
            for item in entries {
                let child = format!("{}/{}", cur, item.name.to_string_lossy());
                stack.push(child);
            }
        }
    }
    total
}

// 递归恢复目录辅助函数（事务内变体：与配额调整/trash 行删除同一事务提交）
async fn restore_dir_recursive_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    username: &str,
    sb: &crate::fsutil::Sandbox,
    dir_rel: &str,
    parent_path: &str,
) -> Result<(), String> {
    let dir_name = dir_rel.rsplit('/').next().unwrap_or("").to_string();
    let _ =
        sqlx::query("INSERT INTO files (username, name, parent_path, is_dir) VALUES (?, ?, ?, 1)")
            .bind(username)
            .bind(&dir_name)
            .bind(parent_path)
            .execute(&mut **tx)
            .await;

    let new_parent = if parent_path.is_empty() {
        dir_name.clone()
    } else {
        format!("{}/{}", parent_path, dir_name)
    };
    if let Ok(entries) = sb.read_dir(dir_rel) {
        for item in entries {
            let child_rel = format!("{}/{}", dir_rel, item.name.to_string_lossy());
            if item.is_dir() {
                Box::pin(restore_dir_recursive_tx(
                    tx,
                    username,
                    sb,
                    &child_rel,
                    &new_parent,
                ))
                .await?;
            } else if item.is_file() {
                let size_mb = item.size as f64 / crate::handlers::utils::BYTES_PER_MB_F64;
                let _ = sqlx::query("INSERT INTO files (username, name, parent_path, is_dir, size_mb) VALUES (?, ?, ?, 0, ?)")
                    .bind(username)
                    .bind(item.name.to_string_lossy().into_owned())
                    .bind(&new_parent)
                    .bind(size_mb)
                    .execute(&mut **tx)
                    .await;
            }
            // 符号链接条目跳过（不恢复，防越界）
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

    // P2-14：持有该 uuid 的互斥锁，防止与后台到期清理竞态
    let _guard = trash_lock(&uuid).await;

    // openat2 沙箱删除（root = .trash，uuid 由 DB 生成，无用户输入）
    let sb = match crate::fsutil::Sandbox::new(crate::constants::TRASH_DIR) {
        Ok(s) => s,
        Err(_) => return Err(AppError::internal("回收站不可用")),
    };
    // P1-8：物理删除失败 → 不删 DB 行、补 tracing::error!（此前 let _ = 吞错，
    // 删行后留下磁盘孤儿持续占盘）。文件已不存在视为成功（幂等：孤儿行可收敛）。
    let exists = sb.try_exists(&uuid).unwrap_or(false);
    if exists {
        let phys_result = if sb.metadata(&uuid).map(|m| m.is_dir()).unwrap_or(false) {
            sb.remove_dir_all(&uuid)
        } else {
            sb.remove_file(&uuid)
        };
        if let Err(e) = phys_result {
            tracing::error!("[Trash] 永久删除物理失败 uuid={}: {}", uuid, e);
            return Err(AppError::internal("回收站删除失败，请稍后重试"));
        }
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
///
/// P1-8 设计选择（注释说明）：采用「先物理全部成功，再单事务批量 DELETE」。
/// 文件系统删除无法回滚，无法与 SQLite 事务混合作业，因此：
/// 1. 先逐个物理删除（每 uuid 持 P2-14 互斥锁）；任一物理删除失败立即
///    中止并**不删任何 DB 行**（失败项的行保留，磁盘不产生孤儿）；
///    已被物理删除但行保留的项，下次重试时 try_exists=false 幂等收敛。
/// 2. 全部物理删除成功后，才在单个事务中批量删除 DB 行（整体原子）。
///
/// 返回 AppResult：物理失败时向调用方上报错误，避免误报“已清空”
async fn clear_trash_physical(pool: &sqlx::SqlitePool, username: &str) -> AppResult<usize> {
    let rows = sqlx::query("SELECT id, trash_uuid FROM trash WHERE username = ?")
        .bind(username)
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::internal_log("查询回收站", e))?;
    if rows.is_empty() {
        return Ok(0);
    }
    let sb = match crate::fsutil::Sandbox::new(crate::constants::TRASH_DIR) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("[Trash] 打开回收站沙箱失败: {}", e);
            return Ok(0);
        }
    };

    let count = rows.len();
    // P2-14：一次性获取全部待删 uuid 的互斥锁并持有到函数结束，
    // 使整段清空流程（物理删除 + DB 行删除）与还原/到期清理完全互斥，
    // 避免两遍之间被并发操作插入造成不一致。
    let mut guards = Vec::with_capacity(count);
    for row in &rows {
        let trash_uuid: String = row.get("trash_uuid");
        guards.push(trash_lock(&trash_uuid).await);
    }
    // 第一遍：逐个物理删除。任一失败 → 中止，一行都不删。
    for row in &rows {
        let trash_uuid: String = row.get("trash_uuid");
        let exists = sb.try_exists(&trash_uuid).unwrap_or(false);
        if !exists {
            continue; // 文件已不存在（孤儿行/上次部分失败），视为成功，重试幂等收敛
        }
        let phys_result = if sb
            .metadata(&trash_uuid)
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            sb.remove_dir_all(&trash_uuid)
        } else {
            sb.remove_file(&trash_uuid)
        };
        if let Err(e) = phys_result {
            tracing::error!("[Trash] 清空物理删除失败 uuid={}: {}", trash_uuid, e);
            return Err(AppError::internal("回收站清空失败，请稍后重试"));
        }
    }

    // 第二遍：全部物理删除成功 → 单事务批量删除 DB 行（整体原子提交）
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| AppError::internal_log("开启清空事务", e))?;
    for row in &rows {
        let id: i64 = row.get("id");
        sqlx::query("DELETE FROM trash WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| AppError::internal_log("删除回收站行", e))?;
    }
    tx.commit()
        .await
        .map_err(|e| AppError::internal_log("提交清空事务", e))?;
    Ok(count)
}

// 清空回收站（当前用户所有项目）
#[tracing::instrument(skip_all)]
pub async fn clear_trash(
    Extension(pool): Extension<sqlx::SqlitePool>,
    Extension(session): Extension<UserSession>,
) -> AppResult<(StatusCode, &'static str)> {
    let count = clear_trash_physical(&pool, &session.username).await?;

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
    let sb = match crate::fsutil::Sandbox::new(crate::constants::TRASH_DIR) {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };

    for row in rows {
        let id: i64 = row.get("id");
        let trash_uuid: String = row.get("trash_uuid");

        // P2-14：与用户还原/永久删除/清空互斥，防止误删正在还原的文件
        let _guard = trash_lock(&trash_uuid).await;

        // P1-8：物理删除成功（或文件已不存在）才删 DB 行；失败保留行 + 记错误，
        // 避免磁盘孤儿持续占盘（删除前检查 try_exists 幂等处理上次部分失败）
        let exists = sb.try_exists(&trash_uuid).unwrap_or(false);
        if exists {
            let phys_result = if sb
                .metadata(&trash_uuid)
                .map(|m| m.is_dir())
                .unwrap_or(false)
            {
                sb.remove_dir_all(&trash_uuid)
            } else {
                sb.remove_file(&trash_uuid)
            };
            if let Err(e) = phys_result {
                tracing::error!("[Trash] 到期清理物理删除失败 uuid={}: {}", trash_uuid, e);
                continue; // 不删行，等待下次清理重试
            }
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
    let clear_result = clear_trash_physical(&pool, &session.username).await;
    match clear_result {
        Ok(count) => {
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
            // 全部成功 → 回收站为空
            AppTemplate(TrashListFragment { items: vec![] })
        }
        Err(_) => {
            // P1-8：物理删除失败，DB 行保留；渲染真实剩余列表而非空列表
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
    }
}
