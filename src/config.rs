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

// serde default 函数
fn default_server_host() -> String { "0.0.0.0".into() }
fn default_server_port() -> u16 { 3000 }
fn default_database_url() -> String { "sqlite:cloud_disk.db".into() }
fn default_session_days() -> i64 { 7 }
fn default_temp_cleanup_hours() -> u64 { 24 }
fn default_trash_cleanup_days() -> u32 { 30 }
fn default_trash_cleanup_interval_hours() -> u64 { 24 }
fn default_quota_mb() -> i64 { 10240 }
fn default_deepseek_api_base() -> String { "https://api.deepseek.com".into() }
fn default_deepseek_model() -> String { "deepseek-v4-flash".into() }

impl Default for Config {
    fn default() -> Self {
        Self {
            server_host: default_server_host(),
            server_port: default_server_port(),
            database_url: default_database_url(),
            upload_limit_mb: 5 * 1024,
            session_days: default_session_days(),
            temp_cleanup_hours: default_temp_cleanup_hours(),
            trash_cleanup_days: default_trash_cleanup_days(),
            trash_cleanup_interval_hours: default_trash_cleanup_interval_hours(),
            default_quota_mb: default_quota_mb(),
            admin_password: None,
            guest_password: None,
            deepseek_api_base: default_deepseek_api_base(),
            deepseek_model: default_deepseek_model(),
            deepseek_api_key: None,
        }
    }
}

/// 手动加载 .env 文件 — 逐行解析 KEY=VALUE 注入进程环境
fn load_dotenv_manual() {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => { eprintln!("[Config] 无法获取当前目录: {}", e); return; }
    };
    let env_path = cwd.join(".env");

    let content = match std::fs::read_to_string(&env_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("[Config] 未找到 .env，使用默认配置");
            return;
        }
        Err(e) => {
            eprintln!("[Config] 读取 {} 失败: {}", env_path.display(), e);
            return;
        }
    };

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(eq_pos) = trimmed.find('=') {
            let key = trimmed[..eq_pos].trim();
            let value = trimmed[eq_pos + 1..].trim().trim_matches('"');
            std::env::set_var(key, value);
            if key == "PINAS_ADMIN_PASSWORD" {
                let masked = if value.len() > 3 {
                    format!("{}***{}", &value[..3], &value[value.len()-1..])
                } else { "***".into() };
                eprintln!("[Config]   PINAS_ADMIN_PASSWORD={}", masked);
            }
        }
    }
}

impl Config {
    pub fn from_env() -> Result<Self, config::ConfigError> {
        load_dotenv_manual();

        let settings = config::Config::builder()
            .add_source(config::Environment::with_prefix("PINAS"))
            .build()?;

        settings.try_deserialize().or_else(|e| {
            // 反序列化失败通常是 .env 缺少必填字段 — 现在都有 #[serde(default)] 了不应失败
            // 保留兜底以便向后兼容
            eprintln!("[Config] 反序列化失败（使用默认值）: {}", e);
            Ok(Config::default())
        })
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

        std::env::remove_var("PINAS_ADMIN_PASSWORD");
        std::env::remove_var("PINAS_SERVER_HOST");
        std::env::remove_var("PINAS_SERVER_PORT");
        std::env::remove_var("PINAS_DATABASE_URL");
    }
}
