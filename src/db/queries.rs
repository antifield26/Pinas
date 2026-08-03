// ====== 共享数据库查询辅助函数 ======
// 提取 handler 中重复的 SQL 模式，减少内联 SQL 字符串拼接

use sqlx::{SqlitePool, Row};

/// 更新目录子树中所有子记录的 parent_path（事务内调用）
/// old_prefix: 旧的完整路径前缀
/// new_prefix: 新的完整路径前缀
pub async fn update_child_paths(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    username: &str, old_prefix: &str, new_prefix: &str,
) -> Result<(), sqlx::Error> {
    // 更新直接子节点
    sqlx::query("UPDATE files SET parent_path = ? WHERE username = ? AND parent_path = ?")
        .bind(new_prefix).bind(username).bind(old_prefix)
        .execute(&mut **tx).await?;

    // 更新深层子节点: parent_path LIKE old_prefix/%
    sqlx::query(
        "UPDATE files SET parent_path = ? || SUBSTR(parent_path, ? + 1) WHERE username = ? AND parent_path LIKE ?"
    )
    .bind(new_prefix).bind(old_prefix.len() as i64).bind(username)
    .bind(format!("{}/%", old_prefix))
    .execute(&mut **tx).await?;

    Ok(())
}

// ====== 用户查询 ======

/// 获取用户密码哈希、角色和密码修改标志
pub async fn get_user_auth(pool: &SqlitePool, username: &str) -> Result<Option<(String, String, bool)>, sqlx::Error> {
    sqlx::query("SELECT password, role, must_change_pwd FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(pool)
        .await
        .map(|r| r.map(|row| {
            let pwd: String = row.get("password");
            let role: String = row.get("role");
            let must_change: i64 = row.get("must_change_pwd");
            (pwd, role, must_change != 0)
        }))
}

/// 检查用户是否存在
pub async fn user_exists(pool: &SqlitePool, username: &str) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE username = ?")
        .bind(username)
        .fetch_one(pool)
        .await
        .map(|count: i64| count > 0)
}

/// 获取用户配额信息
pub async fn get_user_quota(pool: &SqlitePool, username: &str) -> Result<Option<(i64, i64)>, sqlx::Error> {
    sqlx::query_as("SELECT used_mb, quota_mb FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(pool)
        .await
}

/// 更新用户已用容量（重新计算 files 表中所有文件大小）
pub async fn recalc_user_used_mb(pool: &SqlitePool, username: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE users SET used_mb = COALESCE((SELECT SUM(size_mb) FROM files WHERE username = ? AND is_dir = 0), 0) WHERE username = ?"
    )
    .bind(username).bind(username)
    .execute(pool)
    .await
    .map(|_| ())
}

// ====== 会话查询 ======

/// 创建用户会话
pub async fn create_session(
    pool: &SqlitePool,
    token_hash: &str, username: &str, role: &str, expires_at: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO sessions (token, username, role, expires_at) VALUES (?, ?, ?, ?)")
        .bind(token_hash).bind(username).bind(role).bind(expires_at)
        .execute(pool).await
        .map(|_| ())
}

/// 删除会话
pub async fn delete_session(pool: &SqlitePool, token_hash: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM sessions WHERE token = ?")
        .bind(token_hash).execute(pool).await
        .map(|_| ())
}

/// 清理过期会话
pub async fn clean_expired_sessions(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM sessions WHERE expires_at <= datetime('now')")
        .execute(pool).await
        .map(|_| ())
}

// ====== 文件查询 ======

/// 检查文件是否已存在（用于秒传去重）
pub async fn file_exists_by_identifier(
    pool: &SqlitePool, username: &str, identifier: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM files WHERE username = ? AND identifier = ?")
        .bind(username).bind(identifier)
        .fetch_one(pool)
        .await
        .map(|count: i64| count > 0)
}

/// 获取用户在指定目录下的文件和子目录列表
pub async fn list_user_files(
    pool: &SqlitePool, username: &str, parent_path: &str,
) -> Result<Vec<FileRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, username, name, parent_path, is_dir, size_mb, identifier, created_at
         FROM files WHERE username = ? AND parent_path = ?
         ORDER BY is_dir DESC, name ASC"
    )
    .bind(username).bind(parent_path)
    .fetch_all(pool).await
}

/// 文件列表行（用于 FromRow 映射）
#[derive(sqlx::FromRow, Debug, Clone)]
pub struct FileRow {
    pub id: i64,
    pub username: String,
    pub name: String,
    pub parent_path: String,
    pub is_dir: i64,
    pub size_mb: f64,
    pub identifier: Option<String>,
    pub created_at: Option<String>,
}

/// 获取用户在指定目录下的条目数
pub async fn count_user_files(
    pool: &SqlitePool, username: &str, parent_path: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM files WHERE username = ? AND parent_path = ?"
    )
    .bind(username).bind(parent_path)
    .fetch_one(pool).await
}

/// 插入文件记录
pub async fn insert_file_record(
    pool: &SqlitePool,
    username: &str, name: &str, parent_path: &str,
    is_dir: bool, size_mb: f64, identifier: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO files (username, name, parent_path, is_dir, size_mb, identifier) VALUES (?, ?, ?, ?, ?, ?)"
    )
    .bind(username).bind(name).bind(parent_path)
    .bind(if is_dir { 1 } else { 0 })
    .bind(size_mb).bind(identifier)
    .execute(pool).await
    .map(|_| ())
}

// ====== 用户计数 ======

/// 获取总用户数
pub async fn count_users(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(pool).await
}
