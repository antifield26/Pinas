// ====== 共享数据库查询辅助函数 ======
// 提取 handler 中重复的 SQL 模式，减少内联 SQL 字符串拼接

use sqlx::SqlitePool;

/// 检查文件/目录是否存在于指定用户的指定路径
pub async fn file_exists(
    pool: &SqlitePool, username: &str, name: &str, parent_path: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM files WHERE username = ? AND name = ? AND parent_path = ?"
    )
    .bind(username).bind(name).bind(parent_path)
    .fetch_one(pool).await
    .map(|c| c > 0)
}

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

/// 获取用户已用存储容量（MB，四舍五入）
pub async fn get_user_used_mb(pool: &SqlitePool, username: &str) -> Result<i64, sqlx::Error> {
    let used: f64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(size_mb), 0.0) FROM files WHERE username = ? AND is_dir = 0"
    )
    .bind(username).fetch_one(pool).await?;
    Ok(used.round() as i64)
}

/// 在事务内原子更新用户已用容量
pub async fn update_user_quota_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, username: &str, delta_mb: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE users SET used_mb = MAX(0, used_mb + ?) WHERE username = ?")
        .bind(delta_mb).bind(username)
        .execute(&mut **tx).await?;
    Ok(())
}
