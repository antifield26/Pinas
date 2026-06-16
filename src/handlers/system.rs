use axum::{
    extract::Extension,
    http::StatusCode,
    response::{IntoResponse, Json},
};
use pinas_core::UserSession;

pub async fn health_check(
    Extension(pool): Extension<sqlx::SqlitePool>,
) -> impl IntoResponse {
    match sqlx::query_scalar::<_, i64>("SELECT 1").fetch_one(&pool).await {
        Ok(1) => {
            let body = serde_json::json!({
                "status": "healthy",
                "database": "connected",
                "timestamp": chrono::Utc::now().to_rfc3339()
            });
            (StatusCode::OK, Json(body)).into_response()
        }
        Ok(_) => (StatusCode::SERVICE_UNAVAILABLE, "数据库异常").into_response(),
        Err(e) => {
            tracing::error!("健康检查数据库连接失败: {}", e);
            (StatusCode::SERVICE_UNAVAILABLE, "数据库连接失败").into_response()
        }
    }
}

pub async fn get_system_status(
    Extension(session): Extension<UserSession>,
) -> impl IntoResponse {
    if session.role != crate::constants::ROLE_ADMIN {
        return (StatusCode::FORBIDDEN, Json(serde_json::json!({ "error": "管理员权限不足" }))).into_response();
    }

    // CPU 温度
    let cpu_temp = if let Ok(temp_str) = tokio::fs::read_to_string("/sys/class/thermal/thermal_zone0/temp").await {
        temp_str.trim().parse::<f32>().unwrap_or(0.0) / 1000.0
    } else {
        0.0
    };

    // 内存信息
    let (mut mem_total_mb, mut mem_avail_mb) = (0, 0);
    if let Ok(mem_str) = tokio::fs::read_to_string("/proc/meminfo").await {
        for line in mem_str.lines() {
            if line.starts_with("MemTotal:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    mem_total_mb = parts[1].parse::<u64>().unwrap_or(0) / 1024;
                }
            } else if line.starts_with("MemAvailable:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    mem_avail_mb = parts[1].parse::<u64>().unwrap_or(0) / 1024;
                }
            }
        }
    }
    let memory_used_mb = mem_total_mb.saturating_sub(mem_avail_mb);

    // CPU 使用率
    let read_cpu_ticks = || async {
        if let Ok(stat_str) = tokio::fs::read_to_string("/proc/stat").await {
            if let Some(first_line) = stat_str.lines().next() {
                let parts: Vec<&str> = first_line.split_whitespace().collect();
                if parts.len() >= 8 {
                    let user: u64 = parts[1].parse().unwrap_or(0);
                    let nice: u64 = parts[2].parse().unwrap_or(0);
                    let system: u64 = parts[3].parse().unwrap_or(0);
                    let idle: u64 = parts[4].parse().unwrap_or(0);
                    let iowait: u64 = parts[5].parse().unwrap_or(0);
                    let irq: u64 = parts[6].parse().unwrap_or(0);
                    let softirq: u64 = parts[7].parse().unwrap_or(0);
                    let active = user + nice + system + irq + softirq;
                    let total = active + idle + iowait;
                    return Some((active, total));
                }
            }
        }
        None
    };

    let mut cpu_usage = 0.0;
    if let Some((active1, total1)) = read_cpu_ticks().await {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        if let Some((active2, total2)) = read_cpu_ticks().await {
            let total_diff = total2.saturating_sub(total1) as f32;
            let active_diff = active2.saturating_sub(active1) as f32;
            if total_diff > 0.0 {
                cpu_usage = (active_diff / total_diff) * 100.0;
            }
        }
    }

    (StatusCode::OK, Json(serde_json::json!({
        "cpu_usage": cpu_usage,
        "cpu_temp": cpu_temp,
        "memory_used_mb": memory_used_mb,
        "memory_total_mb": mem_total_mb,
    }))).into_response()
}