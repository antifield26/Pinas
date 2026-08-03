// 密码学函数已迁移至 pinas-core::crypto，此处仅做重导出以保持向后兼容
pub use pinas_core::{generate_random_password, hash_password, verify_password};

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

pub fn is_allowed_mime(data: &[u8]) -> bool {
    if data.is_empty() {
        return true;
    }
    if let Some(kind) = infer::get(data) {
        let mime = kind.mime_type();
        if mime.starts_with("application/x-executable")
            || mime.starts_with("application/x-sharedlib")
        {
            return false;
        }
    }
    true
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

pub fn safe_join_sandbox(base: &std::path::Path, user_raw_path: &str) -> std::path::PathBuf {
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
                return base.to_path_buf();
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
                        "[路径安全] 检测到路径穿越攻击，返回安全回退: component='{}', path='{}'",
                        s,
                        user_raw_path
                    );
                    return base.to_path_buf();
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
                return base.to_path_buf();
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
                            return base.to_path_buf();
                        }
                        break;
                    }
                }
                return result;
            }
        };
        if !canonical_result.starts_with(&canonical_base) {
            tracing::error!(
                "[路径安全] 路径穿越兜底拦截: base={:?}, result={:?}",
                canonical_base,
                canonical_result
            );
            return base.to_path_buf();
        }
    }

    result
}

pub fn user_dir_path(raw: Option<String>) -> String {
    let s = raw.unwrap_or_default().trim().to_owned();
    if s == "/" || s.is_empty() {
        String::new()
    } else {
        s
    }
}

// （路径工具函数保留以备 handler 后续重构使用）

/// 全量重算用户已用容量（回退方案，兼容旧逻辑）
pub async fn update_user_used_mb(
    pool: &sqlx::SqlitePool,
    username: &str,
) -> Result<(), sqlx::Error> {
    tracing::debug!("[配额更新] 开始计算用户 {} 的已用容量", username);
    let used_mb_f64: f64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(size_mb), 0.0) FROM files WHERE username = ? AND is_dir = 0",
    )
    .bind(username)
    .fetch_one(pool)
    .await?;
    let used_mb = (used_mb_f64 + 0.5).round() as i64;
    tracing::debug!(
        "[配额更新] 用户 {} 计算得已用: {:.2} MB -> 取整为 {} MB",
        username,
        used_mb_f64,
        used_mb
    );
    sqlx::query("UPDATE users SET used_mb = ? WHERE username = ?")
        .bind(used_mb)
        .bind(username)
        .execute(pool)
        .await?;
    tracing::debug!("[配额更新] 用户 {} 配额更新成功", username);
    Ok(())
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
        let hash = pinas_core::hash_token(token);
        assert_eq!(hash.len(), 64);
        assert_eq!(hash, pinas_core::hash_token(token));
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
        // .. 组件现在拒绝并返回 base（安全回退）
        let result = safe_join_sandbox(base, "user/../passwd");
        assert_eq!(result, base);
        let result2 = safe_join_sandbox(base, "../../../etc/passwd");
        assert_eq!(result2, base);
        let result3 = safe_join_sandbox(base, "folder/./subfolder");
        assert_eq!(result3, base.join("folder").join("subfolder"));
        // 空组件和空白伪装（仍然跳过）
        let result4 = safe_join_sandbox(base, "folder/   /subfolder");
        assert_eq!(result4, base.join("folder").join("subfolder"));
        // Windows 反斜杠分隔符
        let result5 = safe_join_sandbox(base, "folder\\sub");
        assert_eq!(result5, base.join("folder").join("sub"));
        // 绝对路径注入被阻断 — 应返回安全回退
        let result6 = safe_join_sandbox(base, "/etc/shadow");
        assert_eq!(result6, base);
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
        let hash = pinas_core::hash_password(password).unwrap();
        assert!(pinas_core::verify_password(&hash, password));
        assert!(!pinas_core::verify_password(&hash, "wrongpass"));
    }
}
