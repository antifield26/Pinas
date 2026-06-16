pub mod auth;
pub use auth::hash_token;

use serde::{Serialize, Deserialize};

/// 跨应用层和核心安全层透传的通用会话实体
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserSession {
    pub username: String,
    pub role: String,
}