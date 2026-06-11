use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub server_host: String,
    pub server_port: u16,
    pub database_url: String,
    pub upload_limit_mb: u64,
    pub session_days: i64,
    pub temp_cleanup_hours: u64,
    pub trash_cleanup_days: u32,
    pub trash_cleanup_interval_hours: u64,
    pub default_quota_mb: i64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server_host: "0.0.0.0".to_string(),
            server_port: 3000,
            database_url: "sqlite:cloud_disk.db".to_string(),
            upload_limit_mb: 10 * 1024,
            session_days: 7,
            temp_cleanup_hours: 24,
            trash_cleanup_days: 30,
            trash_cleanup_interval_hours: 24,
            default_quota_mb: 10240,
        }
    }
}

impl Config {
    pub fn from_env() -> Result<Self, config::ConfigError> {
        let _ = dotenvy::dotenv();
        let config = config::Config::builder()
            .add_source(config::Environment::default().prefix("PINAS").separator("_"))
            .build()?;
        let mut cfg: Config = config.try_deserialize().unwrap_or_default();
        
        if let Ok(host) = std::env::var("PINAS_SERVER_HOST") {
            cfg.server_host = host;
        }
        if let Ok(port) = std::env::var("PINAS_SERVER_PORT") {
            if let Ok(p) = port.parse() {
                cfg.server_port = p;
            }
        }
        if let Ok(url) = std::env::var("PINAS_DATABASE_URL") {
            cfg.database_url = url;
        }
        if let Ok(limit) = std::env::var("PINAS_UPLOAD_LIMIT_MB") {
            if let Ok(l) = limit.parse() {
                cfg.upload_limit_mb = l;
            }
        }
        if let Ok(quota) = std::env::var("PINAS_DEFAULT_QUOTA_MB") {
            if let Ok(q) = quota.parse() {
                cfg.default_quota_mb = q;
            }
        }
        if let Ok(interval) = std::env::var("PINAS_TRASH_CLEANUP_INTERVAL_HOURS") {
            if let Ok(i) = interval.parse() {
                cfg.trash_cleanup_interval_hours = i;
            }
        }
        Ok(cfg)
    }
}