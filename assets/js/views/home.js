// ====== 主页视图 ======
import { api } from '../api.js';
import { store } from '../state.js';
import { toast } from '../components/toast.js';
import { STATUS_POLL_INTERVAL } from '../constants.js';

// ====== 系统状态监控 ======
let statusInterval = null;

export function startSystemStatusPolling() {
  stopSystemStatusPolling();
  if (!store.isAdmin()) return;
  fetchSystemStatus();
  statusInterval = setInterval(fetchSystemStatus, STATUS_POLL_INTERVAL);
}

export function stopSystemStatusPolling() {
  if (statusInterval) { clearInterval(statusInterval); statusInterval = null; }
}

async function fetchSystemStatus() {
  if (!store.isAdmin()) return;
  try {
    const res = await api.getSystemStatus();
    if (res?.ok) {
      const data = await res.json();
      const card = document.getElementById('system-monitor-card');
      if (card) card.classList.remove('hidden');
      updateEl('sys-cpu-text', `${data.cpu_usage?.toFixed(1)}%`);
      updateBar('sys-cpu-bar', data.cpu_usage);
      updateEl('sys-mem-text', `${data.memory_used_mb} / ${data.memory_total_mb} MB`);
      const memPct = data.memory_total_mb > 0 ? (data.memory_used_mb / data.memory_total_mb) * 100 : 0;
      updateBar('sys-mem-bar', memPct);
      const tempEl = document.getElementById('sys-temp-text');
      if (tempEl) {
        const t = data.cpu_temp?.toFixed(1) || '--';
        tempEl.innerText = `${t} °C`;
        tempEl.className = `font-mono font-bold text-sm px-2 py-0.5 rounded-md ${
          data.cpu_temp > 70 ? 'bg-red-50 text-red-600 animate-pulse dark:bg-red-900/30 dark:text-red-400'
          : data.cpu_temp > 55 ? 'bg-amber-50 text-amber-600 dark:bg-amber-900/30 dark:text-amber-400'
          : 'bg-green-50 text-green-600 dark:bg-green-900/30 dark:text-green-400'}`;
      }
    }
  } catch (e) {
    if (e.status === 401) {
      stopSystemStatusPolling();
      store.logout();
      document.getElementById('login-overlay')?.classList.remove('hidden');
    }
  }
}

function updateEl(id, text) { const el = document.getElementById(id); if (el) el.innerText = text || '--'; }
function updateBar(id, pct) { const el = document.getElementById(id); if (el) el.style.width = `${Math.min(pct || 0, 100)}%`; }

// ====== 链接库功能已迁移至 views/links.js ======
// 链接库现在是独立视图，通过功能导航卡片进入
// 主页仅保留系统监控功能
