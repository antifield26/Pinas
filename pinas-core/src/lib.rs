pub mod auth;
pub mod crypto;

pub use crypto::{hash_token, hash_password, verify_password, generate_random_password};

use serde::{Serialize, Deserialize};

/// 跨应用层和核心安全层透传的通用会话实体
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserSession {
    pub username: String,
    pub role: String,
    pub must_change_pwd: bool,
}
