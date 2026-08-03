use crate::error::{AppError, AppResult};
use axum::{extract::Extension, response::Json};
use pinas_core::UserSession;

// ====== 系统信息采集（消除 get_system_status 和 system_monitor_fragment 重复代码） ======

/// 读取 CPU 温度 (°C)
async fn read_cpu_temp() -> f32 {
    tokio::fs::read_to_string("/sys/class/thermal/thermal_zone0/temp")
        .await
        .ok()
        .and_then(|s| s.trim().parse::<f32>().ok())
        .unwrap_or(0.0)
        / 1000.0
}

/// 读取内存信息，返回 (total_mb, avail_mb)
async fn read_memory() -> (u64, u64) {
    let (mut total, mut avail) = (0u64, 0u64);
    if let Ok(mem) = tokio::fs::read_to_string("/proc/meminfo").await {
        for line in mem.lines() {
            if line.starts_with("MemTotal:") {
                total = line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0)
                    / 1024;
            } else if line.starts_with("MemAvailable:") {
                avail = line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0)
                    / 1024;
            }
        }
    }
    (total, avail)
}

/// 读取一次 CPU 时间片快照，返回 (active_ticks, total_ticks)
fn parse_cpu_ticks(stat_content: &str) -> Option<(u64, u64)> {
    let parts: Vec<u64> = stat_content
        .lines()
        .next()?
        .split_whitespace()
        .skip(1)
        .take(7)
        .filter_map(|v| v.parse().ok())
        .collect();
    if parts.len() >= 7 {
        let active = parts[0] + parts[1] + parts[2] + parts[5] + parts[6];
        let total = active + parts[3] + parts[4];
        Some((active, total))
    } else {
        None
    }
}

async fn read_cpu_ticks() -> Option<(u64, u64)> {
    tokio::fs::read_to_string("/proc/stat")
        .await
        .ok()
        .as_deref()
        .and_then(parse_cpu_ticks)
}

/// 计算 CPU 使用率百分比（两次采样，间隔 200ms）
async fn calc_cpu_usage() -> f32 {
    if let (Some((a1, t1)), Some((a2, t2))) = (read_cpu_ticks().await, {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        read_cpu_ticks().await
    }) {
        let td = t2.saturating_sub(t1) as f32;
        let ad = a2.saturating_sub(a1) as f32;
        if td > 0.0 { (ad / td) * 100.0 } else { 0.0 }
    } else {
        0.0
    }
}

#[tracing::instrument(skip_all)]
pub async fn health_check(
    Extension(pool): Extension<sqlx::SqlitePool>,
) -> AppResult<Json<serde_json::Value>> {
    let db_ok = sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&pool)
        .await
        .is_ok();

    let status = if db_ok { "healthy" } else { "degraded" };

    let body = serde_json::json!({
        "status": status,
        "version": env!("CARGO_PKG_VERSION"),
        "database": if db_ok { "connected" } else { "disconnected" },
        "timestamp": chrono::Utc::now().to_rfc3339()
    });

    if db_ok {
        Ok(Json(body))
    } else {
        Err(AppError::service_unavailable("数据库连接失败"))
    }
}

#[tracing::instrument(skip_all)]
pub async fn get_system_status(
    Extension(session): Extension<UserSession>,
) -> AppResult<Json<serde_json::Value>> {
    if session.role != crate::constants::ROLE_ADMIN {
        return Err(AppError::forbidden("管理员权限不足"));
    }

    let cpu_temp = read_cpu_temp().await;
    let cpu_usage = calc_cpu_usage().await;
    let (mem_total_mb, mem_avail_mb) = read_memory().await;
    let memory_used_mb = mem_total_mb.saturating_sub(mem_avail_mb);

    Ok(Json(serde_json::json!({
        "cpu_usage": cpu_usage,
        "cpu_temp": cpu_temp,
        "memory_used_mb": memory_used_mb,
        "memory_total_mb": mem_total_mb,
    })))
}

// ====== HTMX Fragment ======
use crate::templates::AppTemplate;
use askama::Template;
use axum::response::IntoResponse;

#[derive(Template)]
#[template(path = "components/system_monitor_live.html")]
struct SystemMonitorLive {
    cpu_bar_pct: u32,
    mem_used_mb: u64,
    mem_total_mb: u64,
    mem_bar_pct: u32,
    temp_display: String,
    temp_color_class: String,
}

/// GET /home/system-monitor — 系统状态 HTML 片段（仅管理员）
#[tracing::instrument(skip_all)]
pub async fn system_monitor_fragment(
    Extension(session): Extension<pinas_core::UserSession>,
) -> impl IntoResponse {
    if session.role != crate::constants::ROLE_ADMIN {
        return AppTemplate(SystemMonitorLive {
            cpu_bar_pct: 0,
            mem_used_mb: 0,
            mem_total_mb: 0,
            mem_bar_pct: 0,
            temp_display: "N/A".into(),
            temp_color_class: String::new(),
        })
        .into_response();
    }

    let cpu_temp = read_cpu_temp().await;
    let cpu_bar_pct = calc_cpu_usage().await as u32;
    let (mem_total_mb, mem_avail_mb) = read_memory().await;
    let mem_used_mb = mem_total_mb.saturating_sub(mem_avail_mb);
    let mem_bar_pct = mem_used_mb
        .checked_mul(100)
        .and_then(|v| v.checked_div(mem_total_mb))
        .unwrap_or(0) as u32;

    let temp_display = format!("{:.1} C", cpu_temp);
    let temp_color_class = if cpu_temp > 75.0 {
        "bg-red-100 dark:bg-red-900/30 text-red-700 dark:text-red-400"
    } else if cpu_temp > 60.0 {
        "bg-amber-100 dark:bg-amber-900/30 text-amber-700 dark:text-amber-400"
    } else {
        "bg-green-100 dark:bg-green-900/30 text-green-700 dark:text-green-400"
    };

    AppTemplate(SystemMonitorLive {
        cpu_bar_pct,
        mem_used_mb,
        mem_total_mb,
        mem_bar_pct,
        temp_display,
        temp_color_class: temp_color_class.to_string(),
    })
    .into_response()
}
