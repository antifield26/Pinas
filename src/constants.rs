// ====== 全局常量定义 ======

// --- 目录路径 ---
pub const UPLOADS_DIR: &str = "uploads";
pub const TMP_DIR: &str = "uploads/tmp";
pub const TRASH_DIR: &str = "uploads/tmp/trash";
pub const LOGS_DIR: &str = "logs";
#[allow(dead_code)]
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
#[allow(dead_code)]
pub const MAX_RATE_LIMIT_ENTRIES: usize = 10_000;

// --- 文件大小限制 ---
pub const MAX_BODY_SIZE_BYTES: usize = 100 * 1024 * 1024;
pub const MAX_CHUNK_SIZE_BYTES: u64 = 100 * 1024 * 1024;
pub const MAX_CHUNKS_PER_FILE: i32 = 10_000;
pub const MAX_EDITOR_READ_SIZE_BYTES: u64 = 50 * 1024 * 1024;
pub const MAX_EDIT_SAVE_SIZE_BYTES: usize = 10 * 1024 * 1024;

// --- MIME 检测 ---
#[allow(dead_code)]
pub const MIME_CHECK_STREAM_SIZE: usize = 1024 * 1024;
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

// --- 密码生成 ---
#[allow(dead_code)]
pub const RANDOM_PASSWORD_LEN: usize = 24;

// --- 会话默认 ---
#[allow(dead_code)]
pub const DEFAULT_SESSION_DAYS: i64 = 7;
