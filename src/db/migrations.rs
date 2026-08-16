// ====== 版本化数据库迁移 ======
// 使用 schema_version 表追踪已应用的迁移，避免重复执行

use sqlx::{Row, SqlitePool};

/// 当前最新 schema 版本号
const CURRENT_VERSION: i32 = 11;

/// 执行所有待迁移的 schema 变更。
/// SQLite DDL 事务化：整批迁移一个事务，失败整体回滚，杜绝半迁移状态；
/// 错误向上传播由 main 处理（日志 + 非零退出），不再 panic=abort 直接炸进程
pub async fn run(pool: &SqlitePool) -> Result<(), Box<dyn std::error::Error>> {
    let mut tx = pool.begin().await?;

    // 确保版本表存在
    sqlx::query("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER PRIMARY KEY)")
        .execute(&mut *tx)
        .await?;

    // 获取当前版本
    let current: i32 = sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) FROM schema_version")
        .fetch_one(&mut *tx)
        .await
        .unwrap_or(0);

    // L10 修复：数据库版本高于本二进制支持的版本时显式报错退出——
    // 历史实现静默跳过（旧程序跑新库），行为不可预期
    if current > CURRENT_VERSION {
        drop(tx);
        return Err(format!(
            "数据库 schema 版本 {current} 高于本二进制支持的 {CURRENT_VERSION}，请升级程序或回滚数据库"
        )
        .into());
    }

    // 按版本号递增执行迁移
    if current < 1 {
        apply_v1_migrations(&mut tx).await?;
        sqlx::query("INSERT INTO schema_version (version) VALUES (1)")
            .execute(&mut *tx)
            .await?;
        tracing::info!("[DB] Schema 迁移到 v1 完成");
    }

    if current < 2 {
        apply_v2_migrations(&mut tx).await?;
        sqlx::query("INSERT INTO schema_version (version) VALUES (2)")
            .execute(&mut *tx)
            .await?;
        tracing::info!("[DB] Schema 迁移到 v2 完成");
    }

    if current < 3 {
        apply_v3_migrations(&mut tx).await?;
        sqlx::query("INSERT INTO schema_version (version) VALUES (3)")
            .execute(&mut *tx)
            .await?;
        tracing::info!("[DB] Schema 迁移到 v3 完成");
    }

    if current < 4 {
        apply_v4_migrations(&mut tx).await?;
        sqlx::query("INSERT INTO schema_version (version) VALUES (4)")
            .execute(&mut *tx)
            .await?;
        tracing::info!("[DB] Schema 迁移到 v4 完成");
    }

    if current < 5 {
        apply_v5_migrations(&mut tx).await?;
        sqlx::query("INSERT INTO schema_version (version) VALUES (5)")
            .execute(&mut *tx)
            .await?;
        tracing::info!("[DB] Schema 迁移到 v5 完成");
    }

    if current < 6 {
        apply_v6_migrations(&mut tx).await?;
        sqlx::query("INSERT INTO schema_version (version) VALUES (6)")
            .execute(&mut *tx)
            .await?;
        tracing::info!("[DB] Schema 迁移到 v6 完成");
    }

    if current < 7 {
        apply_v7_migrations(&mut tx).await?;
        sqlx::query("INSERT INTO schema_version (version) VALUES (7)")
            .execute(&mut *tx)
            .await?;
        tracing::info!("[DB] Schema 迁移到 v7 完成");
    }

    if current < 8 {
        apply_v8_migrations(&mut tx).await?;
        sqlx::query("INSERT INTO schema_version (version) VALUES (8)")
            .execute(&mut *tx)
            .await?;
        tracing::info!("[DB] Schema 迁移到 v8 完成");
    }

    if current < 9 {
        apply_v9_migrations(&mut tx).await?;
        sqlx::query("INSERT INTO schema_version (version) VALUES (9)")
            .execute(&mut *tx)
            .await?;
        tracing::info!("[DB] Schema 迁移到 v9 完成");
    }

    if current < 10 {
        apply_v10_migrations(&mut tx).await?;
        sqlx::query("INSERT INTO schema_version (version) VALUES (10)")
            .execute(&mut *tx)
            .await?;
        tracing::info!("[DB] Schema 迁移到 v10 完成");
    }

    if current < 11 {
        apply_v11_migrations(&mut tx).await?;
        sqlx::query("INSERT INTO schema_version (version) VALUES (11)")
            .execute(&mut *tx)
            .await?;
        tracing::info!("[DB] Schema 迁移到 v11 完成");
    }

    tx.commit().await?;

    tracing::debug!(
        "[DB] Schema 版本: {} (最新: {})",
        current.max(CURRENT_VERSION),
        CURRENT_VERSION
    );
    Ok(())
}

/// v1: 初始兼容性列迁移（仅对缺少列的表添加，检查 PRAGMA table_info）
async fn apply_v1_migrations(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), sqlx::Error> {
    // files 表：identifier 列
    let cols: Vec<String> = sqlx::query("PRAGMA table_info(files)")
        .fetch_all(&mut **tx)
        .await
        .map(|rows| rows.iter().filter_map(|r| r.try_get("name").ok()).collect())
        .unwrap_or_default();
    if !cols.iter().any(|c| c == "identifier") {
        sqlx::query("ALTER TABLE files ADD COLUMN identifier TEXT")
            .execute(&mut **tx)
            .await?;
    }

    // todos 表：新列
    let todo_cols: Vec<String> = sqlx::query("PRAGMA table_info(todos)")
        .fetch_all(&mut **tx)
        .await
        .map(|rows| rows.iter().filter_map(|r| r.try_get("name").ok()).collect())
        .unwrap_or_default();
    if !todo_cols.iter().any(|c| c == "is_all_day") {
        sqlx::query("ALTER TABLE todos ADD COLUMN is_all_day INTEGER NOT NULL DEFAULT 1")
            .execute(&mut **tx)
            .await?;
    }
    if !todo_cols.iter().any(|c| c == "start_time") {
        sqlx::query("ALTER TABLE todos ADD COLUMN start_time TEXT")
            .execute(&mut **tx)
            .await?;
    }
    if !todo_cols.iter().any(|c| c == "end_time") {
        sqlx::query("ALTER TABLE todos ADD COLUMN end_time TEXT")
            .execute(&mut **tx)
            .await?;
    }

    // shares 表：download_count 列
    let share_cols: Vec<String> = sqlx::query("PRAGMA table_info(shares)")
        .fetch_all(&mut **tx)
        .await
        .map(|rows| rows.iter().filter_map(|r| r.try_get("name").ok()).collect())
        .unwrap_or_default();
    if !share_cols.iter().any(|c| c == "download_count") {
        sqlx::query("ALTER TABLE shares ADD COLUMN download_count INTEGER DEFAULT 0")
            .execute(&mut **tx)
            .await?;
    }

    // users 表：quota_mb, used_mb, must_change_pwd 列
    let user_cols: Vec<String> = sqlx::query("PRAGMA table_info(users)")
        .fetch_all(&mut **tx)
        .await
        .map(|rows| rows.iter().filter_map(|r| r.try_get("name").ok()).collect())
        .unwrap_or_default();
    if !user_cols.iter().any(|c| c == "quota_mb") {
        sqlx::query("ALTER TABLE users ADD COLUMN quota_mb INTEGER DEFAULT 0")
            .execute(&mut **tx)
            .await?;
    }
    if !user_cols.iter().any(|c| c == "used_mb") {
        sqlx::query("ALTER TABLE users ADD COLUMN used_mb INTEGER DEFAULT 0")
            .execute(&mut **tx)
            .await?;
    }
    if !user_cols.iter().any(|c| c == "must_change_pwd") {
        sqlx::query("ALTER TABLE users ADD COLUMN must_change_pwd INTEGER NOT NULL DEFAULT 0")
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

/// v2: 清理 files.size_mb 列混合类型（TEXT → REAL）
/// SQLite CAST: '-' → 0.0, '0.50' → 0.5, 已有的 REAL 值不变
async fn apply_v2_migrations(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), sqlx::Error> {
    let affected = sqlx::query(
        "UPDATE files SET size_mb = CAST(size_mb AS REAL) WHERE typeof(size_mb) = 'text'",
    )
    .execute(&mut **tx)
    .await
    .map(|r| r.rows_affected())
    .unwrap_or(0);
    tracing::info!("[DB] v2 迁移: size_mb 列 {affected} 行 TEXT → REAL 已转换");
    Ok(())
}

/// v7: 文件搜索索引 — FTS5 trigram 外部内容表(文件名为准,磁盘同步行由触发器维护)
/// trigram tokenizer(SQLite 3.34+)支持中英文子串匹配;查询词 ≤2 字符不命中(trigram 限制),
/// 由查询层降级 LIKE 兜底(见 file_ops.rs bind_list_where)
async fn apply_v7_migrations(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE VIRTUAL TABLE IF NOT EXISTS files_fts USING fts5(
            name, parent_path,
            content='files', content_rowid='id',
            tokenize='trigram'
        )",
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "CREATE TRIGGER IF NOT EXISTS files_ai AFTER INSERT ON files BEGIN
            INSERT INTO files_fts(rowid, name, parent_path) VALUES (new.id, new.name, new.parent_path);
        END",
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "CREATE TRIGGER IF NOT EXISTS files_ad AFTER DELETE ON files BEGIN
            INSERT INTO files_fts(files_fts, rowid, name, parent_path) VALUES('delete', old.id, old.name, old.parent_path);
        END",
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "CREATE TRIGGER IF NOT EXISTS files_au AFTER UPDATE ON files BEGIN
            INSERT INTO files_fts(files_fts, rowid, name, parent_path) VALUES('delete', old.id, old.name, old.parent_path);
            INSERT INTO files_fts(rowid, name, parent_path) VALUES (new.id, new.name, new.parent_path);
        END",
    )
    .execute(&mut **tx)
    .await?;
    // 回填存量数据(不触发 files 表触发器,直接写入 FTS 表)
    // L11 修复：rebuild 失败历史被静默吞掉（搜索静默返回空、无告警）——失败必须可观测
    if let Err(e) = sqlx::query("INSERT INTO files_fts(files_fts) VALUES('rebuild')")
        .execute(&mut **tx)
        .await
    {
        tracing::warn!("[DB] v7 迁移: files_fts rebuild 失败（搜索可能为空）: {e}");
    }
    tracing::info!("[DB] v7 迁移: files_fts 全文索引(trigram)已建立");
    Ok(())
}

/// v6: 重建 conversation_messages 表（AI 消息持久化：聊天历史随对话保存，跨会话可加载）
async fn apply_v6_migrations(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), sqlx::Error> {
    sqlx::query("DROP TABLE IF EXISTS conversation_messages")
        .execute(&mut **tx)
        .await
        .ok();
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS conversation_messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            conversation_id INTEGER NOT NULL,
            role TEXT NOT NULL CHECK (role IN ('user', 'assistant', 'system')),
            content TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            FOREIGN KEY (conversation_id) REFERENCES conversations(id) ON DELETE CASCADE
        )",
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_conversation_messages_conv ON conversation_messages (conversation_id, id)",
    )
    .execute(&mut **tx)
    .await
    .ok();
    tracing::info!("[DB] v6 迁移: 重建 conversation_messages 表（消息持久化）");
    Ok(())
}

/// v5: 移除 conversation_messages 死表（全库无 INSERT/SELECT，AI 消息仅存客户端）
async fn apply_v5_migrations(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), sqlx::Error> {
    sqlx::query("DROP TABLE IF EXISTS conversation_messages")
        .execute(&mut **tx)
        .await
        .ok();
    tracing::info!("[DB] v5 迁移: 移除 conversation_messages 死表");
    Ok(())
}

/// v4: upload_chunks 添加 bytes_received 列(分片阶段磁盘上限核算) + created_at 索引(孤儿行清理)
async fn apply_v4_migrations(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), sqlx::Error> {
    let cols: Vec<String> = sqlx::query("PRAGMA table_info(upload_chunks)")
        .fetch_all(&mut **tx)
        .await
        .unwrap_or_default()
        .iter()
        .map(|r| r.get::<String, _>(1))
        .collect();

    if !cols.iter().any(|c| c == "bytes_received") {
        sqlx::query(
            "ALTER TABLE upload_chunks ADD COLUMN bytes_received INTEGER NOT NULL DEFAULT 0",
        )
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_upload_chunks_created_at ON upload_chunks (created_at)",
    )
    .execute(&mut **tx)
    .await
    .ok();
    tracing::info!("[DB] v4 迁移: upload_chunks 添加 bytes_received 列与 created_at 索引");
    Ok(())
}

/// v11: 会话空闲超时 — sessions.last_active_at（滑动的最后活跃时间）。
/// 认证层惰性刷新 + 空闲超时强制下线；存量会话回填当前时间（升级即视为活跃）
async fn apply_v11_migrations(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), sqlx::Error> {
    let cols: Vec<String> = sqlx::query("PRAGMA table_info(sessions)")
        .fetch_all(&mut **tx)
        .await
        .map(|rows| rows.iter().filter_map(|r| r.try_get("name").ok()).collect())
        .unwrap_or_default();
    if !cols.iter().any(|c| c == "last_active_at") {
        sqlx::query("ALTER TABLE sessions ADD COLUMN last_active_at DATETIME")
            .execute(&mut **tx)
            .await?;
        sqlx::query(
            "UPDATE sessions SET last_active_at = datetime('now') WHERE last_active_at IS NULL",
        )
        .execute(&mut **tx)
        .await?;
    }
    tracing::info!("[DB] v11 迁移: sessions 添加 last_active_at（空闲超时）");
    Ok(())
}

/// v10: 文件操作意图日志（fs_journal）+ 分片尺寸记录（upload_chunks.chunk_sizes）。
/// fs_journal 支撑 rename/move/trash 的崩溃恢复（FS 与 DB 两步非原子的修复）；
/// chunk_sizes 供 merge 完整性校验（截断分片不再产出损坏文件）
async fn apply_v10_migrations(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS fs_journal (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL,
            op TEXT NOT NULL,
            src TEXT NOT NULL,
            dst TEXT NOT NULL,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )",
    )
    .execute(&mut **tx)
    .await?;

    let cols: Vec<String> = sqlx::query("PRAGMA table_info(upload_chunks)")
        .fetch_all(&mut **tx)
        .await
        .map(|rows| rows.iter().filter_map(|r| r.try_get("name").ok()).collect())
        .unwrap_or_default();
    if !cols.iter().any(|c| c == "chunk_sizes") {
        sqlx::query("ALTER TABLE upload_chunks ADD COLUMN chunk_sizes TEXT")
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

/// v9: FTS5 搜索大小写不敏感化——trigram 默认区分大小写，搜 "report" 命中不了 "Report.pdf"。
/// 重建虚拟表 + 触发器 + rebuild（NAS 规模秒级）；幂等：检查现有 tokenizer 配置
async fn apply_v9_migrations(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), sqlx::Error> {
    let sql_row: Option<String> =
        sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE name = 'files_fts'")
            .fetch_optional(&mut **tx)
            .await
            .unwrap_or(None);
    let needs_rebuild = match sql_row {
        Some(sql) => !sql.contains("case_sensitive 0"),
        None => false, // 表不存在：v7 尚未执行（新库），v7 将创建默认配置 → 需重建
    };
    if needs_rebuild {
        let start = std::time::Instant::now();
        sqlx::query("DROP TRIGGER IF EXISTS files_ai")
            .execute(&mut **tx)
            .await?;
        sqlx::query("DROP TRIGGER IF EXISTS files_ad")
            .execute(&mut **tx)
            .await?;
        sqlx::query("DROP TRIGGER IF EXISTS files_au")
            .execute(&mut **tx)
            .await?;
        sqlx::query("DROP TABLE IF EXISTS files_fts")
            .execute(&mut **tx)
            .await?;
        sqlx::query(
            "CREATE VIRTUAL TABLE files_fts USING fts5(
                name, parent_path,
                content='files', content_rowid='id',
                tokenize='trigram case_sensitive 0'
            )",
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            "CREATE TRIGGER files_ai AFTER INSERT ON files BEGIN
                INSERT INTO files_fts(rowid, name, parent_path) VALUES (new.id, new.name, new.parent_path);
            END",
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            "CREATE TRIGGER files_ad AFTER DELETE ON files BEGIN
                INSERT INTO files_fts(files_fts, rowid, name, parent_path) VALUES('delete', old.id, old.name, old.parent_path);
            END",
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            "CREATE TRIGGER files_au AFTER UPDATE ON files BEGIN
                INSERT INTO files_fts(files_fts, rowid, name, parent_path) VALUES('delete', old.id, old.name, old.parent_path);
                INSERT INTO files_fts(rowid, name, parent_path) VALUES (new.id, new.name, new.parent_path);
            END",
        )
        .execute(&mut **tx)
        .await?;
        if let Err(e) = sqlx::query("INSERT INTO files_fts(files_fts) VALUES('rebuild')")
            .execute(&mut **tx)
            .await
        {
            tracing::warn!("[DB] v9 迁移: files_fts rebuild 失败（搜索可能为空）: {e}");
        }
        tracing::info!(
            "[DB] v9 迁移: files_fts 大小写不敏感重建完成（{}ms）",
            start.elapsed().as_millis()
        );
    }
    Ok(())
}

/// v8: 媒体访问令牌表 — 替代会话 token 走 URL 查询串（/api/media/?token=）。
/// 短时效 + 路径限定：预览页签发的令牌只能访问指定路径前缀，泄露影响受限
async fn apply_v8_migrations(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS media_tokens (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL,
            token_hash TEXT NOT NULL UNIQUE,
            path_prefix TEXT NOT NULL,
            created_at TEXT DEFAULT (datetime('now')),
            expires_at TEXT NOT NULL
        )",
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_media_tokens_expires ON media_tokens (expires_at)")
        .execute(&mut **tx)
        .await
        .ok();
    tracing::info!("[DB] v8 迁移: media_tokens 表（短时效路径限定媒体令牌）已建立");
    Ok(())
}

/// v3: user_settings 添加 temperature 和 max_tokens 列
async fn apply_v3_migrations(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), sqlx::Error> {
    let cols: Vec<String> = sqlx::query("PRAGMA table_info(user_settings)")
        .fetch_all(&mut **tx)
        .await
        .unwrap_or_default()
        .iter()
        .map(|r| r.get::<String, _>(1))
        .collect();

    if !cols.iter().any(|c| c == "temperature") {
        sqlx::query("ALTER TABLE user_settings ADD COLUMN temperature REAL NOT NULL DEFAULT 0.7")
            .execute(&mut **tx)
            .await?;
    }
    if !cols.iter().any(|c| c == "max_tokens") {
        sqlx::query(
            "ALTER TABLE user_settings ADD COLUMN max_tokens INTEGER NOT NULL DEFAULT 4096",
        )
        .execute(&mut **tx)
        .await?;
    }
    tracing::info!("[DB] v3 迁移: user_settings 添加 temperature 和 max_tokens 列");
    Ok(())
}
