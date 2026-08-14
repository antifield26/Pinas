// ====== 全局常量定义 ======

// --- 目录路径 ---
pub const UPLOADS_DIR: &str = "uploads";
pub const TMP_DIR: &str = "uploads/tmp";
// 回收站必须位于 TMP_DIR 之外：临时分片清扫按 mtime 清空 uploads/tmp 下的所有条目，
// 若回收站在其内，30 天保留期会被压缩到 ~24 小时（历史数据丢失事故）
pub const TRASH_DIR: &str = "uploads/.trash";
/// 旧版回收站位置（v1.6.0 及以前），启动迁移用
pub const LEGACY_TRASH_DIR: &str = "uploads/tmp/trash";
/// WebDAV MOVE 覆盖的位移目标暂存区（不在 TMP_DIR 内——24h 临时清扫不得触碰；
/// 崩溃残留由启动恢复任务 recover_dav_disp 还原）
pub const DAV_DISP_DIR: &str = "uploads/.dav_disp";
pub const LOGS_DIR: &str = "logs";
pub const STATIC_DIR: &str = "static";
pub const ASSETS_DIR: &str = "assets";

// --- 用户角色 ---
pub const ROLE_ADMIN: &str = "admin";
pub const ROLE_USER: &str = "user";

// --- 速率限制 ---
pub const LOGIN_RATE_LIMIT_ATTEMPTS: u32 = 10;
pub const LOGIN_RATE_LIMIT_WINDOW_SECS: u64 = 60;
pub const REGISTER_RATE_LIMIT_ATTEMPTS: u32 = 3;
pub const REGISTER_RATE_LIMIT_WINDOW_SECS: u64 = 3600;
pub const MAX_RATE_LIMIT_ENTRIES: usize = 10_000;

// --- 文件大小限制 ---
pub const MAX_BODY_SIZE_BYTES: usize = 100 * 1024 * 1024;
pub const MAX_CHUNK_SIZE_BYTES: u64 = 100 * 1024 * 1024;
pub const MAX_CHUNKS_PER_FILE: i32 = 10_000;
/// 单用户未合并临时分片总字节上限（防分片阶段耗尽磁盘的 DoS）
pub const PENDING_CHUNKS_CAP_BYTES: u64 = 5 * 1024 * 1024 * 1024;
pub const MAX_EDITOR_READ_SIZE_BYTES: u64 = 50 * 1024 * 1024;
pub const MAX_EDIT_SAVE_SIZE_BYTES: usize = 10 * 1024 * 1024;

// --- MIME 检测 ---
pub const MIME_HEADER_BUF_SIZE: usize = 512;

// --- 数据库 ---
pub const DB_MAX_CONNECTIONS: u32 = 16;
pub const DB_BUSY_TIMEOUT_SECS: u64 = 10;

// --- 分页 ---
pub const DEFAULT_PAGE_SIZE: i64 = 50;
pub const MAX_PAGE_SIZE: i64 = 200;

// --- 后台任务间隔 ---
pub const RATE_LIMIT_CLEANUP_INTERVAL_SECS: u64 = 600;
pub const RATE_LIMIT_CLEANUP_AGE_SECS: u64 = 300;
pub const LOG_CLEANUP_INTERVAL_SECS: u64 = 86_400;
pub const LOG_RETENTION_DAYS: u64 = 7;
