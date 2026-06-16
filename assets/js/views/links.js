// ====== 链接库视图（独立页面，从主页导航进入） ======
import { api } from '../api.js';
import { store } from '../state.js';
import { toast } from '../components/toast.js';
import { confirmDialog, promptDialog } from '../components/modal.js';
import { escapeHtml, icon } from '../utils.js';

export async function fetchLinks() {
  if (!store.isLoggedIn()) return;
  try {
    const res = await api.getLinks();
    if (res.ok) {
      store.set('links', await res.json());
      renderLinks();
    }
  } catch (e) { console.error('[Links] 加载链接失败', e); }
}

/** 仅允许 http/https 协议的 URL，阻止 javascript: 等危险协议 */
function safeHref(url) {
  const lower = (url || '').trim().toLowerCase();
  if (lower.startsWith('http://') || lower.startsWith('https://')) return url;
  return '#'; // 危险协议退回空链接
}

function renderLinks() {
  const container = document.getElementById('links-view-container');
  if (!container) return;
  const links = store.get('links');
  if (!links.length) {
    container.innerHTML = '<div class="text-center text-gray-400 dark:text-gray-400 col-span-full py-8">暂无链接，点击"添加链接"创建</div>';
    return;
  }
  container.innerHTML = links.map(link => `
    <div class="bg-gray-50 dark:bg-gray-700/50 rounded-lg p-3 flex items-center justify-between group hover:shadow transition">
      <a href="${escapeHtml(safeHref(link.url))}" target="_blank" rel="noopener noreferrer" class="flex items-center gap-2 flex-1 min-w-0">
        <span class="text-xl">${escapeHtml(link.icon || '🔗')}</span>
        <span class="text-sm font-medium text-gray-800 dark:text-gray-200 truncate">${escapeHtml(link.title)}</span>
      </a>
      <div class="flex items-center gap-0.5 shrink-0 transition">
        <button data-action="editLink" data-id="${link.id}" class="w-8 h-8 flex items-center justify-center rounded-lg text-amber-600 bg-gray-100 dark:bg-gray-700 hover:bg-amber-50 dark:hover:bg-amber-900/40 transition" title="编辑">
          ${icon('edit')}
        </button>
        <button data-action="deleteLink" data-id="${link.id}" class="w-8 h-8 flex items-center justify-center rounded-lg text-red-600 bg-gray-100 dark:bg-gray-700 hover:bg-red-50 dark:hover:bg-red-900/40 transition" title="删除">
          ${icon('trash')}
        </button>
      </div>
    </div>`).join('');
}

// 事件委托：链接操作按钮
document.getElementById('links-view-container')?.addEventListener('click', (e) => {
  const btn = e.target.closest('[data-action]');
  if (!btn) return;
  const id = parseInt(btn.dataset.id, 10);
  if (btn.dataset.action === 'editLink') editLink(id);
  else if (btn.dataset.action === 'deleteLink') deleteLink(id);
});

export async function addLink() {
  const title = await promptDialog('请输入链接标题：', '', '添加链接');
  if (!title) return;
  const url = await promptDialog('请输入链接URL（以 http:// 或 https:// 开头）：', '', '添加链接');
  if (!url) return;
  const iconEmoji = await promptDialog('请输入图标 Emoji（可选）：', '', '添加链接');

  try {
    const res = await api.createLink(title, url, iconEmoji || null);
    if (res.ok) { toast('链接添加成功', 'success'); await fetchLinks(); }
    else { const err = await res.text(); console.error('[Links] 添加失败:', err); toast(`添加失败: ${err}`, 'error'); }
  } catch (e) { console.error('[Links] 添加异常:', e); toast('网络错误', 'error'); }
}

export async function editLink(id) {
  console.log('[Links] editLink id=', id, 'type=', typeof id);
  // 先尝试从缓存中查找
  let link = store.get('links').find(l => l.id === id);
  // 缓存未命中则重新获取
  if (!link) {
    try {
      const res = await api.getLinks();
      if (res.ok) {
        store.set('links', await res.json());
        link = store.get('links').find(l => l.id === id);
      }
    } catch (e) { console.error('[Links] 获取链接列表失败:', e); }
  }
  if (!link) {
    console.error('[Links] 未找到链接 id=', id);
    toast('未找到该链接，可能已被删除', 'warning');
    await fetchLinks();
    return;
  }

  console.log('[Links] 找到链接:', link.title);

  const newTitle = await promptDialog('修改标题：', link.title, '编辑链接');
  if (newTitle === null) return;
  const newUrl = await promptDialog('修改URL：', link.url, '编辑链接');
  if (newUrl === null) return;
  const newIcon = await promptDialog('修改图标：', link.icon || '', '编辑链接');
  if (newIcon === null) return;

  try {
    const res = await api.updateLink(id, newTitle, newUrl, newIcon);
    if (res.ok) { toast('链接已更新', 'success'); await fetchLinks(); }
    else { const err = await res.text(); console.error('[Links] 更新失败:', err); toast(`更新失败: ${err}`, 'error'); }
  } catch (e) { console.error('[Links] 更新异常:', e); toast('网络错误', 'error'); }
}

export async function deleteLink(id) {
  if (!(await confirmDialog('确定要删除这个链接吗？'))) return;
  console.log('[Links] deleteLink id=', id);
  try {
    const res = await api.deleteLink(id);
    if (res.ok) { toast('链接已删除', 'success'); await fetchLinks(); }
    else { const err = await res.text(); console.error('[Links] 删除失败:', err); toast(`删除失败: ${err}`, 'error'); }
  } catch (e) { console.error('[Links] 删除异常:', e); toast('网络错误', 'error'); }
}
