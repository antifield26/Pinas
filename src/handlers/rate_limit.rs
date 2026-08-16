use crate::constants::MAX_RATE_LIMIT_ENTRIES;
use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// 每个限速条目：(当前窗口内计数, 窗口起始时刻, 该条目自身 window)
type RateEntry = (u32, Instant, Duration);

/// 异步内存速率限制器：每个 IP 在 window 内最多允许 max_attempts 次请求
/// 使用 tokio::sync::Mutex 避免阻塞异步运行时的工作线程。
/// 条目值含各自 window（P2-2）：容量清理按条目自身窗口过期——历史实现用
/// 「当前端点传入的 window」统一 retain，register（1h 窗）的键堆满后，
/// login（60s 窗）新键因 retain 清不掉 register 键而全部被限速（跨端点 DoS）。
static RATE_LIMITER: LazyLock<Mutex<HashMap<String, RateEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 检查是否允许请求。返回 true 表示允许，false 表示被限制。
/// - ip: 客户端标识（通常为 IP 地址或用户名）
/// - max_attempts: 时间窗口内允许的最大请求数
/// - window: 时间窗口
pub async fn check_rate_limit(ip: &str, max_attempts: u32, window: Duration) -> bool {
    let mut map = RATE_LIMITER.lock().await;
    let now = Instant::now();

    // 容量保护：map 满时先按各条目自身 window 清理过期条目；清理后仍满则拒绝新键。
    // 不驱逐任意条目——驱逐最旧键会被攻击者利用：大量伪造键挤掉受害者的计数，
    // 重置其窗口实现登录爆破绕过（DoS 放大成 auth 绕过）
    if map.len() >= MAX_RATE_LIMIT_ENTRIES && !map.contains_key(ip) {
        map.retain(|_, (_, last, w)| now.duration_since(*last) <= *w);
        if map.len() >= MAX_RATE_LIMIT_ENTRIES {
            return false;
        }
    }

    let entry = map.entry(ip.to_string()).or_insert((0, now, window));
    if now.duration_since(entry.1) > entry.2 {
        // 窗口过期，重置
        *entry = (1, now, window);
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
pub async fn clean_expired_entries(max_age: Duration) {
    let mut map = RATE_LIMITER.lock().await;
    let now = Instant::now();
    // 按自身 window 过期（优先）叠加上限 max_age 兜底：任一满足即清
    map.retain(|_, (_, last, w)| {
        let cutoff = (*w).min(max_age).max(Duration::from_secs(1));
        now.duration_since(*last) <= cutoff
    });
    // 硬上限保护：若仍严重超标则完全清空（防止异常堆积）
    if map.len() > MAX_RATE_LIMIT_ENTRIES * 2 {
        map.clear();
    }
}
