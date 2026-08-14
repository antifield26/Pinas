use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    #[serde(default = "default_server_host")]
    pub server_host: String,
    #[serde(default = "default_server_port")]
    pub server_port: u16,
    #[serde(default = "default_database_url")]
    pub database_url: String,
    #[serde(default = "default_session_days")]
    pub session_days: i64,
    #[serde(default = "default_temp_cleanup_hours")]
    pub temp_cleanup_hours: u64,
    #[serde(default = "default_trash_cleanup_days")]
    pub trash_cleanup_days: u32,
    #[serde(default = "default_trash_cleanup_interval_hours")]
    pub trash_cleanup_interval_hours: u64,
    #[serde(default = "default_upload_limit_mb")]
    pub upload_limit_mb: i64,
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
    /// 注册开关：serde 默认 false（生产未配置即关闭）；Config::default() 为 true（测试/开发便利）
    #[serde(default = "default_allow_registration")]
    pub allow_registration: bool,
    /// Cookie Secure 标志：None=默认强制 Secure（部署在 CF 隧道后）。
    /// 仅纯 HTTP 局域网场景需显式设 PINAS_COOKIE_SECURE=false
    #[serde(default)]
    pub cookie_secure: Option<bool>,
    /// 密码同步开关：true 时每次启动用 PINAS_*_PASSWORD 覆盖 DB 密码。
    /// 默认 false——否则用户在 UI 改密后会被环境变量在下次重启时悄悄重置
    #[serde(default)]
    pub sync_passwords: bool,
    /// AI 每日配额：每用户每日最多 AI 请求次数（防 guest 等账户烧光全局 API 额度）
    #[serde(default = "default_agent_daily_quota")]
    pub agent_daily_quota: u32,
    /// dsh 反代端口：Some(port) 时在 127.0.0.1 起第二监听，反代 DeepSeek Harness Web UI
    #[serde(default)]
    pub dsh_port: Option<u16>,
    /// dsh 上游地址（仅本地环回，信任栅栏要求）
    #[serde(default = "default_dsh_upstream_url")]
    pub dsh_upstream_url: String,
    /// dsh 公网主机名（如 dsh.antifield.work）：代理注入 Host 头 + 未登录重定向目标
    #[serde(default)]
    pub dsh_public_host: Option<String>,
    /// pinas 公网入口（如 https://drive.antifield.work）：dsh 域下未登录重定向的绝对目标
    #[serde(default)]
    pub drive_public_url: Option<String>,
    /// Cookie Domain 属性：统一登录（drive/dsh 同注册域共享会话）
    #[serde(default)]
    pub cookie_domain: Option<String>,
}

// serde default 函数
fn default_server_host() -> String {
    "0.0.0.0".into()
}
fn default_server_port() -> u16 {
    3000
}
fn default_database_url() -> String {
    "sqlite:cloud_disk.db".into()
}
fn default_session_days() -> i64 {
    7
}
fn default_temp_cleanup_hours() -> u64 {
    24
}
fn default_trash_cleanup_days() -> u32 {
    30
}
fn default_trash_cleanup_interval_hours() -> u64 {
    24
}
fn default_upload_limit_mb() -> i64 {
    100
}
fn default_quota_mb() -> i64 {
    10240
}
fn default_deepseek_api_base() -> String {
    "https://api.deepseek.com".into()
}
fn default_deepseek_model() -> String {
    "deepseek-v4-flash".into()
}
fn default_allow_registration() -> bool {
    false
}
fn default_agent_daily_quota() -> u32 {
    200
}
fn default_dsh_upstream_url() -> String {
    "http://127.0.0.1:3080".into()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server_host: default_server_host(),
            server_port: default_server_port(),
            database_url: default_database_url(),
            session_days: default_session_days(),
            temp_cleanup_hours: default_temp_cleanup_hours(),
            trash_cleanup_days: default_trash_cleanup_days(),
            trash_cleanup_interval_hours: default_trash_cleanup_interval_hours(),
            upload_limit_mb: default_upload_limit_mb(),
            default_quota_mb: default_quota_mb(),
            admin_password: None,
            guest_password: None,
            deepseek_api_base: default_deepseek_api_base(),
            deepseek_model: default_deepseek_model(),
            deepseek_api_key: None,
            // 测试/开发便利：Config::default() 开放注册（生产走 from_env，serde 默认关闭）
            allow_registration: true,
            cookie_secure: None,
            sync_passwords: false,
            agent_daily_quota: default_agent_daily_quota(),
            dsh_port: None,
            dsh_upstream_url: default_dsh_upstream_url(),
            dsh_public_host: None,
            drive_public_url: None,
            cookie_domain: None,
        }
    }
}

/// 手动加载 .env 文件 — 逐行解析 KEY=VALUE 注入进程环境
fn load_dotenv_manual() {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[Config] 无法获取当前目录: {}", e);
            return;
        }
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
            unsafe {
                std::env::set_var(key, value);
            }
            if key == "PINAS_ADMIN_PASSWORD" || key == "PINAS_GUEST_PASSWORD" {
                // 安全：绝不打印密码值的任何部分（systemd 下会进 journald，可被离线猜测）
                eprintln!(
                    "[Config]   {}={}",
                    key,
                    if value.is_empty() {
                        "（空）"
                    } else {
                        "✓ 已设置"
                    }
                );
            }
        }
    }
}

impl Config {
    pub fn from_env() -> Result<Self, config::ConfigError> {
        load_dotenv_manual();

        let settings = config::Config::builder()
            .add_source(config::Environment::with_prefix("PINAS").try_parsing(true))
            .build()?;

        let cfg: Self = settings.try_deserialize()?;
        cfg.validate();
        Ok(cfg)
    }

    /// 启动时校验配置值
    fn validate(&self) {
        if self.server_port == 0 {
            eprintln!("[Config] 警告: server_port=0，将使用随机端口");
        }
        if self.upload_limit_mb == 0 {
            eprintln!("[Config] 警告: upload_limit_mb=0，上传功能将不可用");
        }
        if self.session_days < 1 {
            eprintln!(
                "[Config] 警告: session_days={}，会话将立即过期",
                self.session_days
            );
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_env_prefix_maps_flat_keys() {
        unsafe {
            std::env::set_var("PINAS_ADMIN_PASSWORD", "antifield");
        }
        unsafe {
            std::env::set_var("PINAS_SERVER_HOST", "0.0.0.0");
        }
        unsafe {
            std::env::set_var("PINAS_SERVER_PORT", "3000");
        }
        unsafe {
            std::env::set_var("PINAS_DATABASE_URL", "sqlite:test.db");
        }

        let settings = config::Config::builder()
            .add_source(config::Environment::with_prefix("PINAS").try_parsing(true))
            .build()
            .unwrap();

        assert_eq!(
            settings.get::<String>("admin_password").unwrap(),
            "antifield"
        );

        unsafe {
            std::env::remove_var("PINAS_ADMIN_PASSWORD");
        }
        unsafe {
            std::env::remove_var("PINAS_SERVER_HOST");
        }
        unsafe {
            std::env::remove_var("PINAS_SERVER_PORT");
        }
        unsafe {
            std::env::remove_var("PINAS_DATABASE_URL");
        }
    }
}
