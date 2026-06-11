use argon2::{Argon2, PasswordHash, PasswordVerifier};
use sha2::{Sha256, Digest};
use once_cell::sync::Lazy;
use std::collections::HashSet;
use tokio::io::AsyncReadExt;

pub fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub static BLOCKED_EXTENSIONS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    let mut m = HashSet::new();
    m.insert("exe"); m.insert("bat"); m.insert("sh"); m.insert("msi");
    m.insert("vbs"); m.insert("cmd"); m.insert("com");
    m
});

pub fn is_blocked_extension(file_name: &str) -> bool {
    if let Some(ext) = std::path::Path::new(file_name).extension().and_then(|s| s.to_str()) {
        return BLOCKED_EXTENSIONS.contains(ext.to_lowercase().as_str());
    }
    false
}

pub fn is_allowed_mime(data: &[u8]) -> bool {
    if data.is_empty() { return true; }
    if let Some(kind) = infer::get(data) {
        let mime = kind.mime_type();
        if mime.starts_with("application/x-executable") || mime.starts_with("application/x-sharedlib") {
            return false;
        }
    }
    true
}

pub async fn is_allowed_mime_streaming(file_path: &std::path::Path) -> Result<bool, std::io::Error> {
    const CHECK_SIZE: usize = 1024 * 1024;
    let mut file = tokio::fs::File::open(file_path).await?;
    let mut buffer = vec![0u8; CHECK_SIZE];
    let n = file.read(&mut buffer).await?;
    buffer.truncate(n);
    Ok(is_allowed_mime(&buffer))
}

pub fn safe_join_sandbox(base: &std::path::Path, user_raw_path: &str) -> std::path::PathBuf {
    let mut result = base.to_path_buf();
    for component in std::path::Path::new(user_raw_path).components() {
        match component {
            std::path::Component::Normal(p) => {
                if p != ".." && p != "." { result.push(p); }
            }
            _ => {}
        }
    }
    result
}

pub fn user_dir_path(raw: Option<String>) -> String {
    let s = raw.unwrap_or_default().trim().to_string();
    if s == "/" || s.is_empty() { "".to_string() } else { s }
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed_hash) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok(),
        Err(_) => false,
    }
}

pub async fn update_user_used_mb(pool: &sqlx::SqlitePool, username: &str) -> Result<(), sqlx::Error> {
    tracing::debug!("[配额更新] 开始计算用户 {} 的已用容量", username);
    let used_mb_f64: f64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(CAST(size_mb AS REAL)), 0.0) FROM files WHERE username = ? AND is_dir = 0"
    )
    .bind(username)
    .fetch_one(pool)
    .await?;
    let used_mb = (used_mb_f64 + 0.5).round() as i64;
    tracing::debug!("[配额更新] 用户 {} 计算得已用: {:.2} MB -> 取整为 {} MB", username, used_mb_f64, used_mb);
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
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO audit_logs (username, action, target, details, ip_address, user_agent) VALUES (?, ?, ?, ?, ?, ?)"
    )
    .bind(username)
    .bind(action)
    .bind(target)
    .bind(details)
    .bind("unknown")
    .bind("unknown")
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
        let hash = hash_token(token);
        assert_eq!(hash.len(), 64);
        assert_eq!(hash, hash_token(token));
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
        let result = safe_join_sandbox(base, "user/../passwd");
        assert_eq!(result, base.join("user").join("passwd"));
        let result2 = safe_join_sandbox(base, "../../../etc/passwd");
        assert_eq!(result2, base.join("etc").join("passwd"));
        let result3 = safe_join_sandbox(base, "folder/./subfolder");
        assert_eq!(result3, base.join("folder").join("subfolder"));
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
        use argon2::password_hash::{PasswordHasher, SaltString, rand_core::OsRng};
        let salt = SaltString::generate(&mut OsRng);
        let password = "testpass";
        let hash = Argon2::default().hash_password(password.as_bytes(), &salt).unwrap().to_string();
        assert!(verify_password(&hash, password));
        assert!(!verify_password(&hash, "wrongpass"));
    }
}