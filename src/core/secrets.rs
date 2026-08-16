// ====== 静态主密钥与敏感字段落库加密 ======
// P0-3：AI API Key 等敏感字段写入 SQLite 前用 ChaCha20-Poly1305 加密（AEAD，
// 12B 随机 nonce + 密文 + 16B tag），主密钥来源：
//   1. 环境变量 PINAS_MASTER_KEY（64 位 hex）——优先
//   2. 否则 <data_dir>/secret.key 文件（0600，首次运行自动生成）
// 明文密钥仅在进程内存中存在：备份/导出 DB 不再直接泄露密钥；
// 旧版明文值（无 enc:v1: 前缀）读取时原样兼容，下次保存即升级为密文。

use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use std::sync::OnceLock;

const MAGIC: &str = "enc:v1:";
const KEY_FILE: &str = "secret.key";

/// 32 字节主密钥（ChaCha20-Poly1305 的 Key 长度）
pub struct MasterKey([u8; 32]);

static MASTER_KEY: OnceLock<MasterKey> = OnceLock::new();

fn hex_to_bytes(hex: &str) -> Option<[u8; 32]> {
    let hex = hex.trim();
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out[i] = ((hi << 4) | lo) as u8;
    }
    Some(out)
}

fn load_or_create_key_file() -> Result<[u8; 32], String> {
    let path = std::path::Path::new(KEY_FILE);
    if let Ok(content) = std::fs::read_to_string(path) {
        if let Some(bytes) = hex_to_bytes(&content) {
            return Ok(bytes);
        }
        // 文件损坏时绝不静默重新生成：旧密文会全部不可解（等同数据丢失），宁可拒绝启动
        return Err(format!(
            "{KEY_FILE} 内容非法（应为 64 位 hex），拒绝启动以防已有密文不可解"
        ));
    }
    // 首次运行：生成新密钥并写入文件（显式 0600，防同机其他用户读取）
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| format!("主密钥随机数生成失败: {e}"))?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(path, format!("{hex}\n")).map_err(|e| format!("写入 {KEY_FILE} 失败: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    tracing::info!(
        "已生成主密钥文件 {KEY_FILE}（0600）。请将其纳入备份——丢失将无法解密已保存的密钥"
    );
    Ok(bytes)
}

/// 初始化主密钥（main 在切换工作目录后、构建路由前调用；可重复调用，幂等）
pub fn init_master_key() -> Result<(), String> {
    if MASTER_KEY.get().is_some() {
        return Ok(());
    }
    let key = if let Ok(env) = std::env::var("PINAS_MASTER_KEY") {
        let env = env.trim();
        if env.is_empty() {
            return Err("PINAS_MASTER_KEY 为空".to_string());
        }
        hex_to_bytes(env).ok_or_else(|| "PINAS_MASTER_KEY 必须是 64 位 hex".to_string())?
    } else {
        load_or_create_key_file()?
    };
    let _ = MASTER_KEY.set(MasterKey(key));
    tracing::info!(
        "[Secrets] 主密钥就绪（{}）",
        if std::env::var("PINAS_MASTER_KEY").is_ok() {
            "环境变量"
        } else {
            KEY_FILE
        }
    );
    Ok(())
}

/// 获取主密钥（未初始化时 panic——main 必须先调用 init_master_key）
pub fn master_key() -> &'static MasterKey {
    MASTER_KEY
        .get()
        .expect("主密钥未初始化：main 必须在构建路由前调用 secrets::init_master_key()")
}

/// 测试兜底：路由构建时若主密钥未初始化则生成临时密钥（测试进程内可用，不落盘）
pub fn ensure_for_tests() {
    if MASTER_KEY.get().is_none() {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("RNG failure");
        let _ = MASTER_KEY.set(MasterKey(bytes));
    }
}

/// 加密敏感字符串 → "enc:v1:" + base64(nonce || ciphertext || tag)
pub fn encrypt_secret(plain: &str) -> String {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&master_key().0));
    let mut nonce_bytes = [0u8; 12];
    getrandom::fill(&mut nonce_bytes).expect("RNG failure");
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plain.as_bytes())
        .expect("ChaCha20-Poly1305 加密不会失败");
    let mut out = Vec::with_capacity(12 + ct.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    format!(
        "{MAGIC}{}",
        base64::engine::general_purpose::STANDARD.encode(&out)
    )
}

/// 解密敏感字符串。无 MAGIC 前缀视为旧版明文（原样返回，迁移兼容）；
/// 密文损坏/主密钥不匹配返回 Err（调用方记录日志并按"未配置"处理）
pub fn decrypt_secret(encoded: &str) -> Result<String, String> {
    let Some(rest) = encoded.strip_prefix(MAGIC) else {
        return Ok(encoded.to_string());
    };
    let raw = base64::engine::general_purpose::STANDARD
        .decode(rest)
        .map_err(|_| "密文 base64 解码失败".to_string())?;
    if raw.len() < 12 {
        return Err("密文长度非法".to_string());
    }
    let (nonce, ct) = raw.split_at(12);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&master_key().0));
    cipher
        .decrypt(Nonce::from_slice(nonce), ct)
        .map_err(|_| "解密失败（主密钥不匹配或数据损坏）".to_string())
        .and_then(|p| String::from_utf8(p).map_err(|_| "解密结果非 UTF-8".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        ensure_for_tests();
        let plain = "sk-abcdef1234567890";
        let enc = encrypt_secret(plain);
        assert!(enc.starts_with(MAGIC));
        assert_ne!(enc, plain);
        // 同一明文两次加密结果不同（随机 nonce）
        assert_ne!(enc, encrypt_secret(plain));
        assert_eq!(decrypt_secret(&enc).unwrap(), plain);
    }

    #[test]
    fn test_legacy_plaintext_passthrough() {
        ensure_for_tests();
        assert_eq!(
            decrypt_secret("sk-legacy-plain").unwrap(),
            "sk-legacy-plain"
        );
        assert_eq!(decrypt_secret("").unwrap(), "");
    }

    #[test]
    fn test_corrupt_ciphertext_rejected() {
        ensure_for_tests();
        assert!(decrypt_secret("enc:v1:!!!not-base64!!!").is_err());
        assert!(decrypt_secret("enc:v1:c2hvcnQ=").is_err()); // <12B
        // 篡改密文（翻转一个字节）→ 认证失败
        let enc = encrypt_secret("secret-value");
        let rest = enc.strip_prefix(MAGIC).unwrap();
        let raw = base64::engine::general_purpose::STANDARD
            .decode(rest)
            .unwrap();
        let last = raw.len() - 1;
        let mut raw2 = raw;
        raw2[last] ^= 0x01;
        let tampered = format!(
            "{MAGIC}{}",
            base64::engine::general_purpose::STANDARD.encode(&raw2)
        );
        assert!(decrypt_secret(&tampered).is_err());
    }

    #[test]
    fn test_hex_parse() {
        let s = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert!(hex_to_bytes(s).is_some());
        assert!(hex_to_bytes(&s[..62]).is_none());
        assert!(
            hex_to_bytes("zz23456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
                .is_none()
        );
    }
}
