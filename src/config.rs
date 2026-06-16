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
            upload_limit_mb: 5 * 1024,  // 5 GB 单文件上限（分块上传）
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
        let _ = dotenvy::dotenv();
        let config = config::Config::builder()
            .add_source(config::Environment::default().prefix("PINAS").separator("_"))
            .build()?;
        let cfg: Config = match config.try_deserialize() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("环境变量解析失败，将使用默认配置。请检查 PINAS_* 环境变量格式是否正确。错误: {}", e);
                Config::default()
            }
        };
        Ok(cfg)
    }
}