// ====== 密码学工具函数 ======
// 集中管理密码哈希、token 哈希、随机密码生成
// 供 db 层和 handlers 层共用，避免循环依赖

use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use sha2::{Digest, Sha256};

/// SHA-256 哈希 token（用于数据库存储）
pub fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

/// Argon2 密码哈希
pub fn hash_password(password: &str) -> Result<String, String> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|e| format!("密码哈希失败: {}", e))
}

/// Argon2 密码验证
pub fn verify_password(hash: &str, password: &str) -> bool {
    Argon2::default()
        .verify_password(password.as_bytes(), hash)
        .is_ok()
}

/// 使用安全随机数生成器生成随机密码
pub fn generate_random_password() -> String {
    const CHARSET: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*";
    const LEN: usize = 24;
    let mut buf = [0u8; LEN];
    getrandom::fill(&mut buf).expect("RNG failure");
    buf.iter()
        .map(|&b| CHARSET[(b as usize) % CHARSET.len()] as char)
        .collect()
}
