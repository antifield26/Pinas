// ====== 核心安全层 ======
// 原 pinas-core crate 合并回主 crate（拆分理由不足：无循环依赖，且 core 反而依赖 Web 框架）
// 会话实体 + 认证中间件 + 密码学，供 db 层和 handlers 层共用

pub mod auth;
pub mod crypto;
pub mod secrets;

pub use crypto::{generate_random_password, hash_password, hash_token, verify_password};

use serde::{Deserialize, Serialize};

/// 跨应用层和核心安全层透传的通用会话实体
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserSession {
    pub username: String,
    pub role: String,
    pub must_change_pwd: bool,
}
