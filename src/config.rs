use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub server_host: String,
    pub server_port: u16,
    pub database_url: String,
    #[allow(dead_code)]
    pub upload_limit_mb: u64,
    pub session_days: i64,
    pub temp_cleanup_hours: u64,
    pub trash_cleanup_days: u32,
    pub trash_cleanup_interval_hours: u64,
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

impl Config {
    pub fn from_env() -> Result<Self, config::ConfigError> {
        // 将 .env 加载到进程环境中
        match dotenvy::dotenv() {
            Ok(path) => eprintln!("[Config] 已加载 .env: {}", path.display()),
            Err(e) => match &e {
                _ if e.to_string().contains("not found") || e.to_string().contains("NotFound") => {}
                _ => eprintln!("[Config] .env 加载失败: {}", e),
            },
        }

        // 直接验证关键变量是否在进程环境中
        match std::env::var("PINAS_ADMIN_PASSWORD") {
            Ok(ref pwd) if !pwd.is_empty() => {
                eprintln!("[Config] std::env PINAS_ADMIN_PASSWORD = \"{}\" ({})", pwd, pwd.len());
            }
            Ok(_) => eprintln!("[Config] std::env PINAS_ADMIN_PASSWORD = \"\" (空字符串)"),
            Err(std::env::VarError::NotPresent) => {
                eprintln!("[Config] std::env PINAS_ADMIN_PASSWORD = 未找到");
            }
            Err(e) => eprintln!("[Config] std::env PINAS_ADMIN_PASSWORD 错误: {}", e),
        }

        // BUGFIX: 不使用 separator("_") — config crate 会将它用作嵌套键分隔符，
        // 导致 PINAS_ADMIN_PASSWORD → admin.password (嵌套) 而非 admin_password (扁平)。
        // 默认行为只将 _ 用于前缀分隔，剩余部分直接小写为扁平键名。
        let settings = config::Config::builder()
            .add_source(config::Environment::with_prefix("PINAS"))
            .build()?;

        match settings.try_deserialize::<Config>() {
            Ok(c) => {
                eprintln!("[Config] admin_password = {:?}", c.admin_password.as_ref().map(|p| format!("{}字符", p.len())));
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
        // 核心验证: Environment::with_prefix("PINAS") 去除 PINAS_ 前缀后，
        // 剩余部分直接小写为扁平键名（admin_password），不拆分为 admin.password
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
