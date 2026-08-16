// ====== file_ops：共享核心逻辑（P1-1 拆分） ======
// 文件列表查询/磁盘对账、创建/重命名/移动/回收站核心、图标分类。
// HTTP 入口见 api.rs / fragments.rs；物理文件操作一律经 openat2 沙箱（fsutil）。

use crate::error::{AppError, AppResult};
use crate::handlers::utils::safe_join_sandbox;

// ====== 公共 Helper 函数 ======

/// 构建用户文件的物理路径: `uploads/{username}/{parent}/{name}`
pub(crate) fn user_file_path(username: &str, parent: &str, name: &str) -> String {
    if parent.is_empty() {
        format!("{}/{}", username, name)
    } else {
        format!("{}/{}/{}", username, parent, name)
    }
}

/// 构建完整的目录路径: `{parent}/{name}`，parent 为空时返回 name
pub(crate) fn logical_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{}/{}", parent, name)
    }
}

/// 格式化文件大小用于显示（统一入口）
pub(crate) fn fmt_size(mb: f64, is_dir: bool) -> String {
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
/// P0-4：存在性判定经 openat2 沙箱（越界符号链接路径视为不存在 → 记录被清理）
pub(crate) async fn reconcile_files_on_disk(
    pool: &sqlx::SqlitePool,
    username: &str,
    rows: &[(String, String, bool)], // (name, parent_path, is_dir)
) -> std::collections::HashSet<(String, String)> {
    use std::collections::HashSet;
    let username_owned = username.to_string();
    let owned: Vec<(String, String, bool)> = rows.to_vec();
    let checks = owned.into_iter().map(move |(name, parent_path, is_dir)| {
        let username = username_owned.clone();
        async move {
            let rel = user_file_path(&username, &parent_path, &name);
            let exists = match crate::fsutil::Sandbox::new(crate::constants::UPLOADS_DIR) {
                Ok(sb) => match sb.metadata(&rel) {
                    Ok(meta) => {
                        if is_dir {
                            meta.is_dir()
                        } else {
                            meta.is_file()
                        }
                    }
                    Err(_) => false,
                },
                // 沙箱初始化失败（目录不可用）视为不存在，交由调用方清理 DB 记录
                Err(_) => false,
            };
            ((name, parent_path), exists)
        }
    });
    // L7 修复：join_all 对 1000 行目录瞬时并发 1000 个 blocking stat，
    // 4 核 RPi 上线程风暴——buffer_unordered(64) 限流，吞吐损失可忽略
    use futures_util::StreamExt;
    let results = futures_util::stream::iter(checks)
        .buffer_unordered(64)
        .collect::<Vec<_>>()
        .await;

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
pub(crate) fn normalize_display_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return "/".to_string();
    }
    let segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
    format!("/{}", segments.join("/"))
}

/// MIME 类型规范化（浏览器兼容）
pub(crate) fn normalize_preview_mime(
    file_name: &str,
    default_mime: mime_guess::Mime,
) -> mime_guess::Mime {
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

/// 文件系统操作回滚辅助（沙箱内反向 rename）
pub(crate) fn rollback_rename(sb: &crate::fsutil::Sandbox, src: &str, dst: &str) {
    let _ = sb.rename(dst, src);
}

/// 批量移动回滚：逆序把已移动的条目移回原位（同步 fs，回滚路径可接受阻塞）
pub(crate) fn rollback_batch_moves(
    sb: &crate::fsutil::Sandbox,
    moved: &[(String, String, String)],
) {
    for (_, s, d) in moved.iter().rev() {
        let _ = sb.rename(d, s);
    }
}

// ====== 文件列表查询 ======

/// 查询文件列表并过滤磁盘缺失项
pub(crate) async fn query_files(
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
        pub(crate) name: String,
        size_mb: Option<f64>,
        is_dir: i64,
        pub(crate) parent_path: String,
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
                    icon_kind: file_icon_kind(&r.name, r.is_dir != 0).to_string(),
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

// ====== 1. JSON API: 文件列表 ======

pub(crate) fn bind_list_where(
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

pub(crate) fn create_folder_common(
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
    let old_rel = user_file_path(username, parent, old_name);
    let new_rel = user_file_path(username, parent, new_name);
    let sb = crate::fsutil::Sandbox::new(crate::constants::UPLOADS_DIR)
        .map_err(|e| AppError::internal_log("打开沙箱", e))?;

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
    if target_row_exists || sb.try_exists(&new_rel).unwrap_or(true) {
        return Err(AppError::conflict("目标名称已存在，请先移动或删除"));
    }

    let old_logical = logical_path(parent, old_name);
    let new_logical = logical_path(parent, new_name);
    let jid =
        crate::handlers::journal::insert(pool, username, "rename", &old_logical, &new_logical)
            .await?;

    if let Err(e) = sb.rename(&old_rel, &new_rel) {
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
        rollback_rename(&sb, &old_rel, &new_rel);
        crate::handlers::journal::remove(pool, jid).await;
        return Err(e);
    }

    crate::handlers::journal::remove(pool, jid).await;
    Ok(())
}

#[tracing::instrument(skip_all)]

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
    let src_rel = user_file_path(username, src_parent, name);
    let dst_rel = user_file_path(username, dst_parent, name);
    let sb = crate::fsutil::Sandbox::new(crate::constants::UPLOADS_DIR)
        .map_err(|e| AppError::internal_log("打开沙箱", e))?;

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
    if target_row_exists || sb.try_exists(&dst_rel).unwrap_or(true) {
        return Err(AppError::conflict("目标目录存在同名文件，请先移动或删除"));
    }

    // 意图日志先行（M1）：崩溃后启动重放补齐
    let src_logical = logical_path(src_parent, name);
    let dst_logical = logical_path(dst_parent, name);
    let jid = crate::handlers::journal::insert(pool, username, "move", &src_logical, &dst_logical)
        .await?;

    if let Err(e) = sb.rename(&src_rel, &dst_rel) {
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
        rollback_rename(&sb, &src_rel, &dst_rel);
        crate::handlers::journal::remove(pool, jid).await;
        return Err(e);
    }

    crate::handlers::journal::remove(pool, jid).await;
    Ok(())
}

#[tracing::instrument(skip_all)]

pub(crate) async fn delete_to_trash(
    pool: &sqlx::SqlitePool,
    username: &str,
    parent_path: &str,
    name: &str,
) -> Result<(), AppError> {
    use uuid::Uuid;
    // 字符串级第一道闸：拒绝 "." / ".." / 路径分隔符（openat2 的 BENEATH 允许
    // 沙箱内 ".." 解析，此处必须先行拦截，防止 "admin/.." 指向整个 uploads 根）
    crate::handlers::utils::validate_name(name)?;
    let full = logical_path(parent_path, name);
    let rel = user_file_path(username, parent_path, name);
    let trash_uuid = Uuid::new_v4().to_string();
    let trash_rel = format!(".trash/{trash_uuid}");
    let sb = crate::fsutil::Sandbox::new(crate::constants::UPLOADS_DIR)
        .map_err(|e| AppError::internal_log("打开沙箱", e))?;

    if sb.try_exists(&rel).unwrap_or(false) {
        // 回收站目录（.trash）经沙箱创建（同 root 下）
        let _ = sb.create_dir_all(".trash");

        // 意图日志先行（M1）：崩溃后启动重放补齐 trash 行 + files 行删除
        // （journal 的 dst 记录 uuid，重放按 TRASH_DIR.join(uuid) 重建物理路径）
        let jid =
            crate::handlers::journal::insert(pool, username, "trash", &full, &trash_uuid).await?;

        if let Err(e) = sb.rename(&rel, &trash_rel) {
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
            let _ = sb.rename(&trash_rel, &rel);
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

pub(crate) struct FileRowData {
    pub(crate) name: String,
    pub(crate) size_display: String,
    pub(crate) is_dir: bool,
    /// 所在逻辑路径（正常浏览 = 当前目录；全局搜索 = 各自所在目录）
    pub(crate) parent_path: String,
    /// 图标类别（folder/image/video/audio/archive/code/file），
    /// 与 partials/icons.html 的 icon 宏名称一一对应
    pub(crate) icon_kind: String,
}

/// 按文件名/目录映射图标类别（与模板 icons.html 宏保持一致）
pub(crate) fn file_icon_kind(name: &str, is_dir: bool) -> &'static str {
    if is_dir {
        return "folder";
    }
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "svg" | "bmp" | "ico" => "image",
        "mp4" | "mkv" | "avi" | "mov" | "webm" => "video",
        "mp3" | "flac" | "wav" | "ogg" | "m4a" => "audio",
        "zip" | "rar" | "7z" | "tar" | "gz" => "archive",
        "rs" | "py" | "js" | "jsx" | "ts" | "tsx" | "sh" | "c" | "cpp" | "java" | "go" | "json"
        | "yml" | "yaml" | "toml" => "code",
        _ => "file",
    }
}
