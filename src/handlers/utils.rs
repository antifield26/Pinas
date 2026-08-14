// 密码学函数来自 crate::core::crypto，此处重导出以保持 handler 层调用简洁
pub use crate::core::{generate_random_password, hash_password, verify_password};

use std::collections::HashSet;
use std::sync::LazyLock;
use tokio::io::AsyncReadExt;

// --- 文件大小工具 ---

/// 1 MB 对应的字节数
pub const BYTES_PER_MB_F64: f64 = 1_048_576.0;

/// 将字节数格式化为 "X.XX" MB 字符串
pub fn bytes_to_mb_string(bytes: u64) -> String {
    format!("{:.2}", bytes as f64 / BYTES_PER_MB_F64)
}

/// 将字节数向上取整为 MB (用于配额检查等场景)
pub fn bytes_to_mb_ceil(bytes: u64) -> i64 {
    (bytes as f64 / BYTES_PER_MB_F64).ceil() as i64
}

/// 增量调整用户已用配额（delta_mb 可负）：热点路径用它替代全表 SUM 重算
/// （update_user_used_mb 每次全量扫描，保留为对账/低频路径）
pub async fn adjust_user_used_mb(pool: &sqlx::SqlitePool, username: &str, delta_mb: i64) {
    if delta_mb == 0 {
        return;
    }
    if let Err(e) = sqlx::query("UPDATE users SET used_mb = MAX(0, used_mb + ?) WHERE username = ?")
        .bind(delta_mb)
        .bind(username)
        .execute(pool)
        .await
    {
        tracing::error!("[Quota] 增量调整失败: {}", e);
    }
}

/// 事务内变体（与文件登记同事务提交，保证配额与 DB 记录原子一致）
pub async fn adjust_user_used_mb_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    username: &str,
    delta_mb: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE users SET used_mb = MAX(0, used_mb + ?) WHERE username = ?")
        .bind(delta_mb)
        .bind(username)
        .execute(&mut **tx)
        .await
        .map(|_| ())
}

pub static BLOCKED_EXTENSIONS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    let mut m = HashSet::new();
    m.insert("exe");
    m.insert("bat");
    m.insert("sh");
    m.insert("msi");
    m.insert("vbs");
    m.insert("cmd");
    m.insert("com");
    m
});

pub fn is_blocked_extension(file_name: &str) -> bool {
    if let Some(ext) = std::path::Path::new(file_name)
        .extension()
        .and_then(|s| s.to_str())
    {
        return BLOCKED_EXTENSIONS.contains(ext.to_lowercase().as_str());
    }
    false
}

/// 转义 LIKE 模式中的通配符（配合 `LIKE ? ESCAPE '\'` 使用），
/// 防止用户输入中的 %/_ 意外匹配其他行或制造昂贵扫描
/// （实现位于 db::queries，此处重导出供 handlers 层统一调用）
pub use crate::db::queries::escape_like;

/// 校验收藏链接 URL：仅允许 http/https 且含主机名。
/// 拒绝 javascript:/data:/vbscript:/file: 等 scheme（`<a href>` 点击即执行 → 存储型 XSS）
/// 与 `//` 无主机形式；拒绝控制字符。
pub fn validate_url(url: &str) -> crate::error::AppResult<()> {
    use crate::error::AppError;
    let u = url.trim();
    if u.is_empty() {
        return Err(AppError::bad_request("URL不能为空"));
    }
    if u.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(AppError::bad_request("URL包含非法字符"));
    }
    let lower = u.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(AppError::bad_request("URL必须以http://或https://开头"));
    }
    // http:// 之后必须有实际主机名（排除 "http://"、"https://"、"http:///x" 这类空主机）
    let rest = &u[u.find("://").unwrap() + 3..];
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() {
        return Err(AppError::bad_request("URL缺少主机名"));
    }
    Ok(())
}

/// 校验文件/文件夹名称合法性(安全白名单)
/// 拒绝:空名、`.`/`..`、路径分隔符、控制字符、以及会破坏 hx-vals JSON 与内联 JS 字符串的引号/尖括号
pub fn validate_name(name: &str) -> crate::error::AppResult<()> {
    use crate::error::AppError;
    let n = name.trim();
    if n.is_empty() {
        return Err(AppError::bad_request("名称不能为空"));
    }
    if n == "." || n == ".." {
        return Err(AppError::bad_request("非法名称"));
    }
    if n.contains('/') || n.contains('\\') {
        return Err(AppError::bad_request("名称不能包含路径分隔符"));
    }
    for c in n.chars() {
        if c.is_control() || matches!(c, '\'' | '"' | '<' | '>') {
            return Err(AppError::bad_request("名称包含非法字符"));
        }
    }
    Ok(())
}

/// 这些 MIME 类型若以内联方式渲染会执行脚本/标记（存储型 XSS 通道），必须强制下载
pub fn is_force_download_mime(mime: &str) -> bool {
    let m = mime.to_lowercase();
    m.starts_with("text/html")
        || m.ends_with("/html")
        || m.ends_with("/svg+xml")
        || m.ends_with("/xml")
        || m.ends_with("/xhtml+xml")
        || m.contains("javascript")
        || m.ends_with("/x-javascript")
        || m.ends_with("/mjs")
}

pub fn is_allowed_mime(data: &[u8]) -> bool {
    if data.is_empty() {
        return true;
    }
    if let Some(kind) = infer::get(data) {
        let mime = kind.mime_type();
        // infer 0.19 的 MIME 家族（map.rs）：ELF/COFF → x-executable，PE EXE/DLL →
        // vnd.microsoft.portable-executable。历史上只拦 x-executable/x-sharedlib，
        // Windows EXE 改名 .txt 即可穿透 MIME 层——补齐 PE/wasm/mach
        if mime.starts_with("application/x-executable")
            || mime.starts_with("application/x-sharedlib")
            || mime == "application/vnd.microsoft.portable-executable"
            || mime == "application/wasm"
            || mime == "application/x-mach-binary"
        {
            return false;
        }
    }
    true
}

/// 签发短时效、路径限定的媒体令牌（30 分钟）：替代会话 token 出现在媒体 URL 中。
/// 泄露影响受限——只能访问签发路径前缀之下的资源，半小时后自动失效。
/// 返回空串表示签发失败（调用方应退化为不签发，媒体 URL 走 Cookie/Bearer 认证）。
pub async fn issue_media_token(
    pool: &sqlx::SqlitePool,
    username: &str,
    path_prefix: &str,
) -> String {
    // 前缀规范化（纵深防御）：去首尾 /、压平空段；含 ".." 直接拒绝（返回空串=不签发）。
    // 路径限定校验在 auth 层按段比较，未规范化的前缀（如 "dir/../x"）会制造歧义
    let mut segs: Vec<&str> = Vec::new();
    for seg in path_prefix.trim_matches('/').split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            tracing::warn!("[MediaToken] 拒绝含 .. 的前缀: {}", path_prefix);
            return String::new();
        }
        segs.push(seg);
    }
    let normalized_prefix = segs.join("/");

    let token = uuid::Uuid::new_v4().to_string();
    let token_hash = crate::core::hash_token(&token);
    let expires_at = chrono::Utc::now()
        .checked_add_signed(chrono::Duration::minutes(30))
        .unwrap_or_else(|| chrono::Utc::now() + chrono::Duration::minutes(30))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    match sqlx::query(
        "INSERT INTO media_tokens (username, token_hash, path_prefix, expires_at) VALUES (?, ?, ?, ?)",
    )
    .bind(username)
    .bind(&token_hash)
    .bind(&normalized_prefix)
    .bind(&expires_at)
    .execute(pool)
    .await
    {
        Ok(_) => token,
        Err(e) => {
            tracing::error!("[Media] 媒体令牌入库失败: {}", e);
            String::new()
        }
    }
}

pub async fn is_allowed_mime_streaming(
    file_path: &std::path::Path,
) -> Result<bool, std::io::Error> {
    const CHECK_SIZE: usize = 1024 * 1024;
    let mut file = tokio::fs::File::open(file_path).await?;
    let mut buffer = vec![0u8; CHECK_SIZE];
    let n = file.read(&mut buffer).await?;
    buffer.truncate(n);
    Ok(is_allowed_mime(&buffer))
}

pub fn safe_join_sandbox(
    base: &std::path::Path,
    user_raw_path: &str,
) -> crate::error::AppResult<std::path::PathBuf> {
    use crate::error::AppError;
    // 统一路径分隔符 (Windows 兼容)
    let normalized = user_raw_path.replace('\\', "/");

    let mut result = base.to_path_buf();
    for component in std::path::Path::new(&normalized).components() {
        match component {
            std::path::Component::ParentDir => {
                tracing::warn!(
                    "[路径安全] 检测到路径穿越攻击 (ParentDir), path='{}'",
                    user_raw_path
                );
                return Err(AppError::bad_request("非法路径"));
            }
            std::path::Component::CurDir => {
                // 跳过 . 组件（无害）
                continue;
            }
            std::path::Component::Normal(p) => {
                // 拒绝 .. 路径穿越（Unix 上 .. 可能作为 Normal 出现），跳过 . 和空白伪装
                let s = p.to_string_lossy();
                if s == ".." {
                    tracing::warn!(
                        "[路径安全] 检测到路径穿越攻击: component='{}', path='{}'",
                        s,
                        user_raw_path
                    );
                    return Err(AppError::bad_request("非法路径"));
                }
                if s == "." || s.trim().is_empty() {
                    tracing::warn!(
                        "[路径安全] 跳过无效路径组件: component='{}', path='{}'",
                        s,
                        user_raw_path
                    );
                    continue;
                }
                result.push(p);
            }
            // 显式拒绝绝对路径和 Windows 盘符前缀
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                tracing::warn!("[路径安全] 阻断绝对路径/盘符注入: path='{}'", user_raw_path);
                return Err(AppError::bad_request("非法路径"));
            }
        }
    }

    // 兜底校验：规范化后必须位于 base 之下
    let canonical_base = match base.canonicalize() {
        Ok(p) => p,
        Err(_) => base.to_path_buf(),
    };

    if canonical_base.exists() {
        // 查找 result 路径树中最近的存在节点，对其进行规范化
        let canonical_result = match result.canonicalize() {
            Ok(p) => p,
            Err(_) => {
                // 目标不存在时，向上查找最近的已存在父目录进行校验
                let mut ancestor = result.clone();
                loop {
                    if !ancestor.pop() {
                        break;
                    }
                    if let Ok(canon) = ancestor.canonicalize() {
                        // 将未创建部分拼接回去
                        let remaining = result
                            .strip_prefix(&ancestor)
                            .unwrap_or(std::path::Path::new(""));
                        let full = canon.join(remaining);
                        if !full.starts_with(&canonical_base) {
                            tracing::error!(
                                "[路径安全] 路径穿越: ancestor={:?}, base={:?}",
                                canon,
                                canonical_base
                            );
                            return Err(AppError::bad_request("非法路径"));
                        }
                        break;
                    }
                }
                return Ok(result);
            }
        };
        if !canonical_result.starts_with(&canonical_base) {
            tracing::error!(
                "[路径安全] 路径穿越兜底拦截: base={:?}, result={:?}",
                canonical_base,
                canonical_result
            );
            return Err(AppError::bad_request("非法路径"));
        }
    }

    Ok(result)
}

pub fn user_dir_path(raw: Option<String>) -> String {
    let s = raw.unwrap_or_default().trim().to_owned();
    if s == "/" || s.is_empty() {
        String::new()
    } else {
        s
    }
}

/// 全量重算用户已用容量。
/// 统一算法:SUM(CEIL(size_mb))——与上传时按文件向上取整累加的语义一致,消除显示漂移。
/// (此前为 (SUM+0.5).round(),与上传的 ceil 增量不同,导致配额显示不一致)
/// 事务化（M4 修复）：先以空写 UPDATE 抢占写锁再读再写——deferred 事务下若先读后写，
/// 并发增量调整可在读后提交、随后被本函数的旧快照覆盖（配额漂移）
pub async fn update_user_used_mb(
    pool: &sqlx::SqlitePool,
    username: &str,
) -> Result<(), sqlx::Error> {
    tracing::debug!("[配额更新] 开始计算用户 {} 的已用容量", username);
    let mut tx = pool.begin().await?;
    // 抢占写锁：确保 SUM 读到的是该事务线性化点之后不会再被其他增量调整修改的状态
    sqlx::query("UPDATE users SET used_mb = used_mb WHERE username = ?")
        .bind(username)
        .execute(&mut *tx)
        .await?;
    let used_mb: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(CEIL(size_mb)), 0) FROM files WHERE username = ? AND is_dir = 0",
    )
    .bind(username)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("UPDATE users SET used_mb = ? WHERE username = ?")
        .bind(used_mb)
        .bind(username)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tracing::debug!("[配额更新] 用户 {} 配额更新成功: {} MB", username, used_mb);
    Ok(())
}

/// 事务内配额预检 + 增量调整（delta_mb 可为负=释放）。
/// 与调用方事务原子提交：SQLite 写锁串行化并发写路径，消除「先检查后写」的 TOCTOU
/// （dav PUT / 回收站恢复 / 编辑器保存共用）。返回调整后的 (used, quota)
pub async fn check_and_adjust_quota_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    username: &str,
    delta_mb: i64,
) -> Result<(i64, i64), crate::error::AppError> {
    use crate::error::AppError;
    use sqlx::Row as _;
    let row = sqlx::query("SELECT used_mb, quota_mb FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| AppError::internal_log("查询配额", e))?;
    let (used, quota) = match row {
        Some(r) => (
            r.try_get::<i64, _>(0)
                .map_err(|e| AppError::internal_log("解析配额", e))?,
            r.try_get::<i64, _>(1)
                .map_err(|e| AppError::internal_log("解析配额", e))?,
        ),
        None => return Err(AppError::not_found("用户不存在")),
    };
    let new_used = used + delta_mb;
    if new_used > quota {
        return Err(AppError::forbidden(format!(
            "存储空间不足，配额 {} MB，已使用 {} MB",
            quota, used
        )));
    }
    sqlx::query("UPDATE users SET used_mb = ? WHERE username = ?")
        .bind(new_used)
        .bind(username)
        .execute(&mut **tx)
        .await
        .map_err(|e| AppError::internal_log("更新配额", e))?;
    Ok((new_used, quota))
}

pub async fn log_audit(
    pool: &sqlx::SqlitePool,
    username: &str,
    action: &str,
    target: Option<&str>,
    details: Option<&str>,
    ip_address: Option<&str>,
    user_agent: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO audit_logs (username, action, target, details, ip_address, user_agent) VALUES (?, ?, ?, ?, ?, ?)"
    )
    .bind(username)
    .bind(action)
    .bind(target)
    .bind(details)
    .bind(ip_address.unwrap_or("-"))
    .bind(user_agent.unwrap_or("-"))
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;
    use tokio::fs::File;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn test_hash_token() {
        let token = "test-token";
        let hash = crate::core::hash_token(token);
        assert_eq!(hash.len(), 64);
        assert_eq!(hash, crate::core::hash_token(token));
    }

    #[test]
    fn test_is_blocked_extension() {
        assert!(is_blocked_extension("virus.exe"));
        assert!(is_blocked_extension("script.bat"));
        assert!(is_blocked_extension("malware.sh"));
        assert!(is_blocked_extension("setup.msi"));
        assert!(is_blocked_extension("evil.vbs"));
        assert!(is_blocked_extension("command.cmd"));
        assert!(is_blocked_extension("program.com"));
        assert!(!is_blocked_extension("document.pdf"));
        assert!(!is_blocked_extension("image.jpg"));
    }

    #[test]
    fn test_is_allowed_mime() {
        let txt_data = b"Hello, world!";
        assert!(is_allowed_mime(txt_data));
        let empty = b"";
        assert!(is_allowed_mime(empty));
    }

    #[test]
    fn test_safe_join_sandbox() {
        let base = std::path::Path::new("/uploads");
        // .. 组件、绝对路径注入 → Err(非法路径)，不再回退到 base
        assert!(safe_join_sandbox(base, "user/../passwd").is_err());
        assert!(safe_join_sandbox(base, "../../../etc/passwd").is_err());
        assert!(safe_join_sandbox(base, "/etc/shadow").is_err());
        // Unix 上 "C:\windows" 归一化为相对路径 "C:/windows"，落在沙箱内（安全）
        assert_eq!(
            safe_join_sandbox(base, "C:\\windows").unwrap(),
            base.join("C:").join("windows")
        );
        // 正常组件/子目录/反斜杠归一化/空白伪装 → Ok
        assert_eq!(
            safe_join_sandbox(base, "folder/./subfolder").unwrap(),
            base.join("folder").join("subfolder")
        );
        assert_eq!(
            safe_join_sandbox(base, "folder/   /subfolder").unwrap(),
            base.join("folder").join("subfolder")
        );
        assert_eq!(
            safe_join_sandbox(base, "folder\\sub").unwrap(),
            base.join("folder").join("sub")
        );
    }

    #[test]
    fn test_validate_name() {
        assert!(validate_name("文档.pdf").is_ok());
        assert!(validate_name("中文目录").is_ok());
        assert!(validate_name("a b-c_1").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("   ").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name(".").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("a\\b").is_err());
        assert!(validate_name("a'b").is_err());
        assert!(validate_name("a\"b").is_err());
        assert!(validate_name("a<b").is_err());
        assert!(validate_name("a>b").is_err());
        assert!(validate_name("a\u{0}b").is_err());
    }

    #[test]
    fn test_user_dir_path() {
        assert_eq!(user_dir_path(Some("/".to_string())), "");
        assert_eq!(user_dir_path(Some("".to_string())), "");
        assert_eq!(user_dir_path(None), "");
        assert_eq!(user_dir_path(Some("folder".to_string())), "folder");
        assert_eq!(user_dir_path(Some("folder/sub".to_string())), "folder/sub");
    }

    #[tokio::test]
    async fn test_is_allowed_mime_streaming() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let mut file = File::create(path).await.unwrap();
        file.write_all(b"some text").await.unwrap();
        let result = is_allowed_mime_streaming(path).await.unwrap();
        assert!(result);
    }

    #[test]
    fn test_verify_password() {
        let password = "testpass";
        let hash = crate::core::hash_password(password).unwrap();
        assert!(crate::core::verify_password(&hash, password));
        assert!(!crate::core::verify_password(&hash, "wrongpass"));
    }
}
