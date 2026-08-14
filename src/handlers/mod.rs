// 模块声明
mod admin;
mod agent;
mod auth;
mod conversations;
mod dav;
mod dsh;
mod file_ops;
mod journal;
mod links;
mod media;
mod minecraft;
mod pages;
pub mod rate_limit;
mod settings;
mod share;
mod system;
mod todos;
mod trash;
mod upload;
mod utils;

// 公共导出
pub use admin::*;
pub use agent::*;
pub use auth::*;
pub use conversations::*;
pub use dav::*;
pub use dsh::*;
pub use file_ops::*;
pub use links::*;
pub use media::*;
pub use minecraft::*;
pub use pages::*;
pub use settings::*;
pub use share::*;
pub use system::*;
pub use todos::*;
pub use trash::*;
pub use upload::*;
pub use utils::*;

// 启动时重放文件操作意图日志（main 调用，先于后台清理任务）
pub use journal::replay as replay_fs_journal;

// 公共 DTO
use serde::Deserialize;

#[derive(Deserialize)]
pub struct BatchDownloadRequest {
    pub names: Vec<String>,
    pub current_path: Option<String>,
}
