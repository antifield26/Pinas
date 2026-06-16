// ====== 用户管理视图（管理员） ======
import { api } from '../api.js';
import { toast } from '../components/toast.js';
import { confirmDialog, promptDialog } from '../components/modal.js';
import { escapeHtml } from '../utils.js';

export async function loadUserList() {
  const tbody = document.getElementById('user-list-body');
  if (!tbody) return;
  tbody.innerHTML = `<tr><td colspan="4" class="p-8 text-center text-gray-400 dark:text-gray-500"><div class="inline-block animate-spin">⏳</div> 加载中...</td></tr>`;

  try {
    const res = await api.listUsers();
    if (res.ok) {
      const users = await res.json();
      if (!users.length) {
        tbody.innerHTML = '<tr><td colspan="4" class="p-8 text-center text-gray-400 dark:text-gray-500">暂无用户</td></tr>';
        return;
      }
      tbody.innerHTML = users.map(user => {
        const pct = user.quota_mb > 0 ? (user.used_mb / user.quota_mb) * 100 : 0;
        const barColor = pct > 85 ? 'bg-red-500' : pct > 70 ? 'bg-amber-500' : 'bg-indigo-600';
        return `
          <tr class="hover:bg-gray-50/80 dark:hover:bg-gray-750 transition">
            <td class="p-4 font-mono text-sm text-gray-800 dark:text-gray-200">${escapeHtml(user.username)}</td>
            <td class="p-4 text-xs text-gray-500 dark:text-gray-400">${user.role === 'admin' ? '管理员' : '普通用户'}</td>
            <td class="p-4 text-xs text-gray-500 dark:text-gray-400">
              ${user.used_mb} / ${user.quota_mb} MB
              <div class="w-24 bg-gray-200 dark:bg-gray-700 rounded-full h-1.5 mt-1">
                <div class="${barColor} h-full rounded-full" style="width:${Math.min(pct, 100)}%"></div>
              </div>
            </td>
            <td class="p-4 text-right space-x-2">
              <button data-action="setQuota" data-username="${escapeHtml(user.username)}" data-quota="${user.quota_mb}"
                class="text-xs font-medium text-blue-600 hover:text-blue-900 dark:text-blue-400 transition">配额</button>
              <button data-action="resetPassword" data-username="${escapeHtml(user.username)}"
                class="text-xs font-medium text-amber-600 hover:text-amber-900 dark:text-amber-400 transition">密码</button>
            </td>
          </tr>`;
      }).join('');
    } else {
      tbody.innerHTML = '<tr><td colspan="4" class="p-8 text-center text-red-400">加载失败</td></tr>';
    }
  } catch (e) {
    if (e.status !== 401) tbody.innerHTML = '<tr><td colspan="4" class="p-8 text-center text-red-400">网络错误</td></tr>';
  }
}

export async function setQuotaPrompt(username, currentQuota) {
  const newQuota = await promptDialog(`请输入用户 ${username} 的新配额（MB）：`, String(currentQuota), '修改配额');
  if (newQuota === null) return;
  const quotaMb = parseInt(newQuota, 10);
  if (isNaN(quotaMb) || quotaMb <= 0) return toast('配额必须是正整数', 'error');

  try {
    const res = await api.setQuota(username, quotaMb);
    if (res.ok) { toast(`用户 ${username} 配额已更新`, 'success'); await loadUserList(); }
    else { const err = await res.text(); toast(`更新失败: ${err}`, 'error'); }
  } catch (e) { if (e.status !== 401) toast('网络错误', 'error'); }
}

export async function resetPasswordPrompt(username) {
  const newPassword = await promptDialog(`请输入 ${username} 的新密码（留空随机生成）：`, '', '重置密码');
  if (newPassword === null) return;

  let finalPassword = newPassword;
  if (!newPassword || !newPassword.trim()) {
    finalPassword = Math.random().toString(36).slice(-8);
    toast(`将使用随机密码: ${finalPassword}`, 'info');
  }

  if (!(await confirmDialog(`确定重置用户 ${username} 的密码为: ${finalPassword} 吗？`))) return;

  try {
    const res = await api.resetUserPassword(username, finalPassword);
    if (res.ok) { toast(`密码重置成功，新密码: ${finalPassword}`, 'success'); }
    else { const err = await res.text(); toast(`重置失败: ${err}`, 'error'); }
  } catch (e) { if (e.status !== 401) toast('网络错误', 'error'); }
}

// 事件委托：用户列表操作按钮（替代不安全的 onclick 内联处理）
document.getElementById('user-list-body')?.addEventListener('click', (e) => {
  const btn = e.target.closest('[data-action]');
  if (!btn) return;
  const { action, username, quota } = btn.dataset;
  if (action === 'setQuota') {
    setQuotaPrompt(username, parseInt(quota, 10));
  } else if (action === 'resetPassword') {
    resetPasswordPrompt(username);
  }
});
