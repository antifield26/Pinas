use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// 限速器最大容量 — 防止伪造 IP 攻击耗尽内存
const MAX_RATE_LIMIT_ENTRIES: usize = 10_000;

/// 简单的内存速率限制器：每个 IP 在 window 内最多允许 max_attempts 次请求
static RATE_LIMITER: LazyLock<Mutex<HashMap<String, (u32, Instant)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 检查是否允许请求。返回 true 表示允许，false 表示被限制。
/// - ip: 客户端标识（通常为 IP 地址）
/// - max_attempts: 时间窗口内允许的最大请求数
/// - window: 时间窗口
pub fn check_rate_limit(ip: &str, max_attempts: u32, window: Duration) -> bool {
    let mut map = RATE_LIMITER.lock().unwrap();
    let now = Instant::now();

    // 容量保护：若 map 超过上限，先强制清理过期条目
    if map.len() >= MAX_RATE_LIMIT_ENTRIES {
        map.retain(|_, (_, last)| now.duration_since(*last) <= window);
        // 清理后仍超限，移除最旧条目
        if map.len() >= MAX_RATE_LIMIT_ENTRIES {
            let oldest_key = map
                .iter()
                .min_by_key(|(_, (_, last))| *last)
                .map(|(k, _)| k.clone());
            if let Some(key) = oldest_key {
                map.remove(&key);
            }
        }
    }

    let entry = map.entry(ip.to_string()).or_insert((0, now));
    if now.duration_since(entry.1) > window {
        // 窗口过期，重置
        *entry = (1, now);
        true
    } else if entry.0 >= max_attempts {
        // 超过限制
        false
    } else {
        entry.0 += 1;
        true
    }
}

/// 定期清理过期条目（后台任务定期调用）
pub fn clean_expired_entries(max_age: Duration) {
    if let Ok(mut map) = RATE_LIMITER.lock() {
        let now = Instant::now();
        map.retain(|_, (_, last)| now.duration_since(*last) < max_age);
        // 硬上限保护：若仍严重超标则完全清空（防止异常堆积）
        if map.len() > MAX_RATE_LIMIT_ENTRIES * 2 {
            map.clear();
        }
    }
}
