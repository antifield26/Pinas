// ====== 分享管理视图 ======
import { api } from '../api.js';
import { store } from '../state.js';
import { toast } from '../components/toast.js';
import { confirmDialog } from '../components/modal.js';
import { escapeHtml, copyToClipboard } from '../utils.js';

let shareListCache = [];

export async function loadShares() {
  try {
    const res = await api.listShares();
    if (res.ok) {
      shareListCache = await res.json();
      renderShares();
    } else { toast('加载分享列表失败', 'error'); }
  } catch (e) { if (e.status !== 401) toast('网络错误', 'error'); }
}

function renderShares() {
  const container = document.getElementById('share-list-container');
  if (!container) return;

  if (!shareListCache.length) {
    container.innerHTML = '<div class="text-center text-gray-400 dark:text-gray-500 py-10">暂无任何分享链接</div>';
    return;
  }

  container.innerHTML = shareListCache.map(share => {
    const shareUrl = `${location.origin}/s/${share.code}`;
    const expires = share.expires_at ? new Date(share.expires_at).toLocaleString() : '永久有效';
    const pwdStatus = share.has_password ? '有密码' : '无密码';
    return `
      <div class="border border-gray-200 dark:border-gray-700 rounded-xl p-4 bg-gray-50/30 dark:bg-gray-750 hover:bg-white dark:hover:bg-gray-700 transition">
        <div class="flex flex-wrap justify-between items-start gap-2">
          <div class="flex-1 min-w-0">
            <div class="font-mono text-xs text-indigo-600 dark:text-indigo-400 break-all">${escapeHtml(share.code)}</div>
            <div class="text-sm font-medium text-gray-800 dark:text-gray-200 mt-1">${escapeHtml(share.file_path)}</div>
            <div class="flex flex-wrap gap-3 text-xs text-gray-500 dark:text-gray-400 mt-2">
              <span>⏱️ ${expires}</span>
              <span>${pwdStatus}</span>
              <span>下载: ${share.download_count || 0}</span>
            </div>
          </div>
          <div class="flex gap-2">
            <button onclick="App.share.copyUrl('${shareUrl}')" class="px-3 py-1 text-xs bg-gray-200 hover:bg-gray-300 dark:bg-gray-600 dark:hover:bg-gray-500 rounded-lg transition">复制</button>
            <button onclick="App.share.deleteShare('${share.code}')" class="px-3 py-1 text-xs bg-red-100 hover:bg-red-200 dark:bg-red-900/30 dark:hover:bg-red-900/50 text-red-700 dark:text-red-400 rounded-lg transition">删除</button>
          </div>
        </div>
      </div>`;
  }).join('');
}

export async function copyUrl(url) {
  const ok = await copyToClipboard(url);
  toast(ok ? '分享链接已复制到剪贴板' : '复制失败，请手动复制', ok ? 'success' : 'error');
}

export async function deleteShare(code) {
  if (!(await confirmDialog('确定要删除这个分享链接吗？'))) return;
  try {
    const res = await api.deleteShare(code);
    if (res.ok) { toast('分享链接已删除', 'success'); await loadShares(); }
    else { toast('删除失败', 'error'); }
  } catch (e) { if (e.status !== 401) toast('网络错误', 'error'); }
}

// ====== 创建分享 ======
export function closeCreateShareModal() {
  const modal = document.getElementById('create-share-modal');
  if (modal) { modal.style.display = 'none'; modal.classList.add('hidden'); }
}

export async function confirmCreateShare() {
  const filePath = document.getElementById('share-path')?.value;
  const isDir = document.getElementById('share-is-dir')?.value === '1';
  const expireHours = document.getElementById('share-expire-hours')?.value;
  const password = document.getElementById('share-password')?.value;

  try {
    const res = await api.createShare(
      filePath, isDir,
      expireHours ? parseInt(expireHours) : null,
      password || null
    );
    if (res.ok) {
      const data = await res.json();
      toast(`分享链接已创建: ${data.code}`, 'success');
      closeCreateShareModal();
      const shareModal = document.getElementById('share-modal');
      if (shareModal?.style.display === 'flex') await loadShares();
    } else {
      const err = await res.text();
      toast(`创建失败: ${err}`, 'error');
    }
  } catch (e) { if (e.status !== 401) toast('网络错误', 'error'); }
}

export function openShareManager() {
  if (!store.isLoggedIn()) return toast('请先登录', 'warning');
  document.getElementById('share-modal')?.classList.remove('hidden');
  const el = document.getElementById('share-modal');
  if (el) el.style.display = 'flex';
  loadShares();
}

export function closeShareModal() {
  const el = document.getElementById('share-modal');
  if (el) { el.style.display = 'none'; el.classList.add('hidden'); }
}
