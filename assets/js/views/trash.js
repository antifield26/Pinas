// ====== 回收站视图 ======
import { api } from '../api.js';
import { store } from '../state.js';
import { toast } from '../components/toast.js';
import { confirmDialog, dangerConfirmDialog } from '../components/modal.js';
import { escapeHtml } from '../utils.js';

export async function loadTrashList() {
  const tbody = document.getElementById('trash-list-body');
  if (tbody) tbody.innerHTML = `<tr><td colspan="3" class="p-8 text-center text-gray-400 dark:text-gray-500"><div class="inline-block animate-spin">⏳</div> 加载中...</td></tr>`;

  try {
    const res = await api.listTrash();
    if (res.ok) {
      const items = await res.json();
      if (!items.length) {
        if (tbody) tbody.innerHTML = '<tr><td colspan="3" class="p-8 text-center text-gray-400 dark:text-gray-500">回收站为空</td></tr>';
        return;
      }
      if (tbody) {
        tbody.innerHTML = items.map(item => `
          <tr class="hover:bg-gray-50/80 dark:hover:bg-gray-750 transition">
            <td class="p-4 font-mono text-sm text-gray-800 dark:text-gray-200">${escapeHtml(item.original_path)}</td>
            <td class="p-4 text-xs text-gray-500 dark:text-gray-400">${new Date(item.deleted_at).toLocaleString()}</td>
            <td class="p-4 text-right space-x-2">
              <button onclick="App.trash.restore(${item.id})" class="text-xs font-medium text-green-600 hover:text-green-900 dark:text-green-400 transition">↩️ 还原</button>
              <button onclick="App.trash.permanentDelete(${item.id})" class="text-xs font-medium text-red-600 hover:text-red-900 dark:text-red-400 transition">永久删除</button>
            </td>
          </tr>`).join('');
      }
    } else {
      if (tbody) tbody.innerHTML = '<tr><td colspan="3" class="p-8 text-center text-red-400">加载失败</td></tr>';
    }
  } catch (e) {
    if (e.status !== 401 && tbody) tbody.innerHTML = '<tr><td colspan="3" class="p-8 text-center text-red-400">网络错误</td></tr>';
  }
}

export async function restore(id) {
  if (!(await confirmDialog('确定要还原此文件/目录吗？'))) return;
  try {
    const res = await api.restoreTrash(id);
    if (res.ok) {
      toast('还原成功', 'success');
      await loadTrashList();
      if (store.get('currentView') === 'drive') {
        const { fetchFiles, fetchQuota } = await import('./drive.js');
        await fetchFiles(store.get('currentPage'));
        await fetchQuota();
      }
    } else {
      const err = await res.text();
      toast(`还原失败: ${err}`, 'error');
    }
  } catch (e) { if (e.status !== 401) toast('网络错误', 'error'); }
}

export async function permanentDelete(id) {
  if (!(await dangerConfirmDialog('永久删除后无法恢复，确定要删除吗？'))) return;
  try {
    const res = await api.deleteTrashPermanent(id);
    if (res.ok) {
      toast('已永久删除', 'success');
      await loadTrashList();
      if (store.get('currentView') === 'drive') {
        const { fetchQuota } = await import('./drive.js');
        await fetchQuota();
      }
    } else {
      const err = await res.text();
      toast(`删除失败: ${err}`, 'error');
    }
  } catch (e) { if (e.status !== 401) toast('网络错误', 'error'); }
}

export async function clearTrash() {
  if (!(await dangerConfirmDialog('确定要永久清空回收站中的所有文件吗？此操作不可恢复！', '清空回收站'))) return;
  try {
    const res = await api.clearTrash();
    if (res.ok) {
      toast('回收站已清空', 'success');
      await loadTrashList();
      if (store.get('currentView') === 'drive') {
        const { fetchQuota } = await import('./drive.js');
        await fetchQuota();
      }
    } else {
      const err = await res.text();
      toast(`清空失败: ${err}`, 'error');
    }
  } catch (e) { if (e.status !== 401) toast('网络错误', 'error'); }
}
