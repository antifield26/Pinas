// ====== 密码学工具函数 ======
// 集中管理密码哈希、token 哈希、随机密码生成
// 供 db 层和 handlers 层共用

use argon2::{
    Algorithm, Argon2, Params, Version,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};
use sha2::{Digest, Sha256};

/// 新哈希参数：Argon2id v19，m=19MiB / t=3 / p=1（OWASP 下限为 t=2，
/// RPi4 核上 t=3 约 150ms，认证延迟可接受，爆破成本提升 50%）
fn new_hasher() -> Argon2<'static> {
    Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(19456, 3, 1, None).expect("argon2 参数固定合法"),
    )
}

/// 验证用 hasher：参数从密码哈希串自带字段解析，与默认值无关（新旧哈希均兼容）
fn verify_hasher() -> Argon2<'static> {
    Argon2::default()
}

/// SHA-256 哈希 token（用于数据库存储）
pub fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

/// Argon2 密码哈希（自动随机盐；参数见 new_hasher）
pub fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    new_hasher()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| format!("密码哈希失败: {}", e))
}

/// Argon2 密码验证（参数随哈希串自描述，不受默认参数变更影响）
pub fn verify_password(hash: &str, password: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => verify_hasher()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
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
