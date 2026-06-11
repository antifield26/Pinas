// 模块声明
mod utils;
mod auth;
mod file_ops;
mod upload;
mod share;
mod trash;
mod admin;
mod media;
mod system;
mod links;

// 公共导出
pub use auth::*;
pub use file_ops::*;
pub use upload::*;
pub use share::*;
pub use trash::*;
pub use admin::*;
pub use media::*;
pub use system::*;
pub use links::*;

// 公共 DTO
use serde::Deserialize;

#[derive(Deserialize)]
pub struct BatchDownloadRequest {
    pub names: Vec<String>,
    pub current_path: Option<String>,
}