// ====== file_ops 模块（P1-1 拆分：core / api / fragments） ======
pub mod api;
pub mod core;
pub mod fragments;

pub use api::*;
pub use fragments::*;
// core 的函数均为 pub(crate)：以 pub(crate) glob 导出供 journal/upload/dav 等跨模块引用
pub(crate) use core::*;
