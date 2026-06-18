use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    #[serde(default = "default_server_host")]
    pub server_host: String,
    #[serde(default = "default_server_port")]
    pub server_port: u16,
    #[serde(default = "default_database_url")]
    pub database_url: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub upload_limit_mb: u64,
    #[serde(default = "default_session_days")]
    pub session_days: i64,
    #[serde(default = "default_temp_cleanup_hours")]
    pub temp_cleanup_hours: u64,
    #[serde(default = "default_trash_cleanup_days")]
    pub trash_cleanup_days: u32,
    #[serde(default = "default_trash_cleanup_interval_hours")]
    pub trash_cleanup_interval_hours: u64,
    #[serde(default = "default_quota_mb")]
    pub default_quota_mb: i64,
    #[serde(default)]
    pub admin_password: Option<String>,
    #[serde(default)]
    pub guest_password: Option<String>,
    #[serde(default = "default_deepseek_api_base")]
    pub deepseek_api_base: String,
    #[serde(default = "default_deepseek_model")]
    pub deepseek_model: String,
    #[serde(default)]
    pub deepseek_api_key: Option<String>,
}

fn default_server_host() -> String { "0.0.0.0".to_string() }
fn default_server_port() -> u16 { 3000 }
fn default_database_url() -> String { "sqlite:cloud_disk.db".to_string() }
fn default_session_days() -> i64 { 7 }
fn default_temp_cleanup_hours() -> u64 { 24 }
fn default_trash_cleanup_days() -> u32 { 30 }
fn default_trash_cleanup_interval_hours() -> u64 { 24 }
fn default_quota_mb() -> i64 { 10240 }
fn default_deepseek_api_base() -> String { "https://api.deepseek.com".to_string() }
fn default_deepseek_model() -> String { "deepseek-v4-flash".to_string() }
impl Default for Config {
    fn default() -> Self {
        Self {
            server_host: "0.0.0.0".to_string(),
            server_port: 3000,
            database_url: "sqlite:cloud_disk.db".to_string(),
            upload_limit_mb: 5 * 1024,
            session_days: 7,
            temp_cleanup_hours: 24,
            trash_cleanup_days: 30,
            trash_cleanup_interval_hours: 24,
            default_quota_mb: 10240,
            admin_password: None,
            guest_password: None,
            deepseek_api_base: default_deepseek_api_base(),
            deepseek_model: default_deepseek_model(),
            deepseek_api_key: None,
        }
    }
}

/// 手动加载 .env 文件 — 逐行解析 KEY=VALUE 并注入进程环境
/// 不使用 dotenvy 以避免平台差异（中文 Windows 下的错误信息匹配等）
fn load_dotenv_manual() {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[Config] 无法获取当前目录: {}", e);
            return;
        }
    };
    eprintln!("[Config] 工作目录: {}", cwd.display());

    // 尝试多个可能的 .env 路径
    let candidates = [
        cwd.join(".env"),
        std::path::PathBuf::from(".env"),
    ];

    let mut loaded = false;
    for path in &candidates {
        match std::fs::read_to_string(path) {
            Ok(content) => {
                eprintln!("[Config] ✓ 读取 .env: {}", path.display());
                for (lineno, line) in content.lines().enumerate() {
                    let trimmed = line.trim();
                    // 跳过空行和注释
                    if trimmed.is_empty() || trimmed.starts_with('#') {
                        continue;
                    }
                    // 解析 KEY=VALUE
                    if let Some(eq_pos) = trimmed.find('=') {
                        let key = trimmed[..eq_pos].trim();
                        let value = trimmed[eq_pos + 1..].trim();
                        // 去掉引号（如果有）
                        let value = value
                            .strip_prefix('"').unwrap_or(value)
                            .strip_suffix('"').unwrap_or(value);
                        std::env::set_var(key, value);

                        if key == "PINAS_ADMIN_PASSWORD" {
                            let masked = if value.len() <= 3 {
                                value.to_string()
                            } else {
                                format!("{}***", &value[..3])
                            };
                            eprintln!("[Config]   L{}: {}={}", lineno + 1, key, masked);
                        }
                    } else if !trimmed.is_empty() {
                        eprintln!("[Config]   L{}: 跳过无效行: {}", lineno + 1, trimmed);
                    }
                }
                loaded = true;
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("[Config] .env 未找到: {}", path.display());
            }
            Err(e) => {
                eprintln!("[Config] 读取 {} 失败: {}", path.display(), e);
            }
        }
    }

    if !loaded {
        eprintln!("[Config] 未找到 .env 文件，使用默认配置");
    }
}

impl Config {
    pub fn from_env() -> Result<Self, config::ConfigError> {
        // 手动加载 .env（避免 dotenvy 平台差异）
        load_dotenv_manual();

        // 直接验证 PINAS_ADMIN_PASSWORD 是否在进程环境中
        match std::env::var("PINAS_ADMIN_PASSWORD") {
            Ok(ref pwd) if !pwd.is_empty() => {
                eprintln!("[Config] std::env PINAS_ADMIN_PASSWORD = \"{}\" ({} 字符)", pwd, pwd.len());
            }
            Ok(_) => eprintln!("[Config] std::env PINAS_ADMIN_PASSWORD = \"\" (空字符串)"),
            Err(_) => eprintln!("[Config] std::env PINAS_ADMIN_PASSWORD = 未找到！"),
        }

        // config crate 从进程环境读取（已由 load_dotenv_manual 注入）
        let settings = config::Config::builder()
            .add_source(config::Environment::with_prefix("PINAS"))
            .build()?;

        match settings.try_deserialize::<Config>() {
            Ok(c) => {
                eprintln!("[Config] admin_password = {:?}", c.admin_password.as_ref().map(|p| format!("{} 字符", p.len())));
                eprintln!("[Config] server_port = {}", c.server_port);
                Ok(c)
            }
            Err(e) => {
                eprintln!("[Config] 反序列化失败: {}", e);
                Ok(Config::default())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_env_prefix_maps_flat_keys() {
        std::env::set_var("PINAS_ADMIN_PASSWORD", "antifield");
        std::env::set_var("PINAS_SERVER_HOST", "0.0.0.0");
        std::env::set_var("PINAS_SERVER_PORT", "3000");
        std::env::set_var("PINAS_DATABASE_URL", "sqlite:test.db");

        let settings = config::Config::builder()
            .add_source(config::Environment::with_prefix("PINAS"))
            .build()
            .unwrap();

        assert_eq!(settings.get::<String>("admin_password").unwrap(), "antifield");
        assert_eq!(settings.get::<String>("server_host").unwrap(), "0.0.0.0");

        std::env::remove_var("PINAS_ADMIN_PASSWORD");
        std::env::remove_var("PINAS_SERVER_HOST");
        std::env::remove_var("PINAS_SERVER_PORT");
        std::env::remove_var("PINAS_DATABASE_URL");
    }
}
