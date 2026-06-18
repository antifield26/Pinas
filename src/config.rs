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
        // 加载 .env 文件（显式处理错误）
        match dotenvy::dotenv() {
            Ok(path) => eprintln!("[Config] 已加载 .env: {}", path.display()),
            Err(e) => {
                // .env 不存在时不报错（首次部署可能没有），但其他错误要报告
                if !matches!(&e, dotenvy::Error::Io(io_err) if io_err.kind() == std::io::ErrorKind::NotFound) {
                    eprintln!("[Config] 警告: .env 加载失败: {}", e);
                }
            }
        }

        // 验证关键环境变量是否已设置
        match std::env::var("PINAS_ADMIN_PASSWORD") {
            Ok(ref pwd) if !pwd.is_empty() => {
                eprintln!("[Config] PINAS_ADMIN_PASSWORD: 已设置 ({} 字符)", pwd.len());
            }
            _ => {
                eprintln!("[Config] PINAS_ADMIN_PASSWORD: 未设置，将自动生成随机密码");
            }
        }

        let config = config::Config::builder()
            .add_source(config::Environment::default().prefix("PINAS").separator("_"))
            .build()?;

        match config.try_deserialize() {
            Ok(c) => Ok(c),
            Err(e) => {
                eprintln!("[Config] 环境变量解析失败，使用默认配置。错误: {}", e);
                eprintln!("[Config] 环境变量格式应为 PINAS_前缀+下划线分隔，如 PINAS_ADMIN_PASSWORD=xxx");
                Ok(Config::default())
            }
        }
    }
}
