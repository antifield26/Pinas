// ====== Drive 视图（文件浏览器核心） ======
import { api } from '../api.js';
import { store } from '../state.js';
import { toast } from '../components/toast.js';
import { confirmDialog, promptDialog, dangerConfirmDialog } from '../components/modal.js';
import { renderFileTable, renderPagination } from '../components/file-table.js';
import { escapeHtml, debounce } from '../utils.js';
import { DEFAULT_PAGE_SIZE } from '../constants.js';
import { openEditor } from './editor.js';
import { playMedia } from './player.js';
import { uploadFile } from './upload.js';

// ====== 文件列表 ======
export async function fetchFiles(page = 1) {
  const keyword = document.getElementById('searchKeyword')?.value.trim() || '';
  const sortBy = document.getElementById('sortBy')?.value || 'name_asc';
  const path = store.get('currentPath');

  try {
    const res = await api.listFiles({ path, search: keyword, sort_by: sortBy, page, page_size: DEFAULT_PAGE_SIZE });
    if (res.ok) {
      const data = await res.json();
      store.setMultiple({
        files: data.items || [],
        totalFiles: data.total || 0,
        totalPages: data.total_pages || 1,
        currentPage: data.page || 1,
      });
      renderAll();
    } else {
      const err = await res.text();
      toast(`加载文件列表失败: ${err}`, 'error');
    }
  } catch (e) {
    if (e.status !== 401) toast('加载文件列表失败', 'error');
  }
}

// 搜索防抖版本
export const fetchFilesDebounced = debounce(() => fetchFiles(1));

function renderAll() {
  const files = store.get('files');
  const path = store.get('currentPath');
  const username = store.get('username');
  const page = store.get('currentPage');
  const totalPages = store.get('totalPages');
  const total = store.get('totalFiles');

  renderBreadcrumbs();
  renderFileTable(files, path, username);
  renderPagination(page, totalPages, total);
  fetchQuota();
}

function renderBreadcrumbs() {
  const bc = document.getElementById('path-breadcrumbs');
  if (!bc) return;
  const path = store.get('currentPath');
  if (!path) { bc.innerHTML = `<span class="text-gray-400">/</span>`; return; }

  const parts = path.split('/');
  let acc = '';
  let html = `<span class="cursor-pointer text-indigo-600 hover:underline font-bold" data-nav="">Root</span>`;
  parts.forEach((p, i) => {
    acc += (i === 0 ? p : '/' + p);
    html += ` <span class="text-gray-300">/</span> <span class="cursor-pointer text-indigo-600 hover:underline" data-nav="${encodeURIComponent(acc)}">${escapeHtml(p)}</span>`;
  });
  bc.innerHTML = html;
}

// ====== 配额 ======
export async function fetchQuota() {
  try {
    const res = await api.getQuota();
    if (res.ok) {
      const data = await res.json();
      const quotaEl = document.getElementById('quota-used');
      const totalEl = document.getElementById('quota-total');
      const bar = document.getElementById('quota-bar');
      if (quotaEl) quotaEl.innerText = data.used_mb;
      if (totalEl) totalEl.innerText = data.quota_mb;
      if (bar) {
        const pct = data.quota_mb > 0 ? (data.used_mb / data.quota_mb) * 100 : 0;
        bar.style.width = `${Math.min(pct, 100)}%`;
        bar.className = 'h-full rounded-full transition-all duration-300 ' + (
          pct > 85 ? 'bg-red-500' : pct > 70 ? 'bg-amber-500' : 'bg-indigo-600'
        );
      }
    }
  } catch (e) { console.error('获取配额失败', e); }
}

// ====== 导航 ======
export function navigateTo(encodedPath) {
  const path = decodeURIComponent(encodedPath);
  store.set('currentPath', path);
  store.clearSelection();
  const newUrl = window.location.pathname + (path ? `?path=${encodedPath}` : '');
  window.history.pushState({ path }, '', newUrl);
  fetchFiles(1);
}

// ====== 操作：事件委托处理器 ======
export async function handleFileAction(action, dataset) {
  switch (action) {
    case 'navigate': navigateTo(dataset.path); break;
    case 'deleteFile': await deleteFileHandler(dataset.filename); break;
    case 'renameFile': await renameFileHandler(dataset.filename); break;
    case 'moveFile': await moveFileHandler(dataset.filename); break;
    case 'openEditor': openEditor(decodeURIComponent(dataset.filename)); break;
    case 'createShare': openCreateShare(decodeURIComponent(dataset.path), dataset.isdir === '1'); break;
    case 'playMedia':
    case 'previewFile': playMedia(decodeURIComponent(dataset.path), dataset.type, dataset.filename); break;
    case 'downloadFile': downloadFile(dataset.url, dataset.filename); break;
  }
}

async function deleteFileHandler(encodedName) {
  const name = decodeURIComponent(encodedName);
  const ok = await confirmDialog(`确定删除 [${name}] 吗？文件将移至回收站。`);
  if (!ok) return;
  try {
    const res = await api.deleteFile(name, store.get('currentPath'));
    if (res.ok) { toast('已移至回收站', 'success'); fetchFiles(store.get('currentPage')); }
    else { const err = await res.text(); toast(`删除失败: ${err}`, 'error'); }
  } catch (e) { if (e.status !== 401) toast('删除请求异常', 'error'); }
}

async function renameFileHandler(encodedName) {
  const oldName = decodeURIComponent(encodedName);
  const newName = await promptDialog(`请输入 [${oldName}] 的新名称:`, oldName, '重命名');
  if (!newName || newName.trim() === oldName) return;
  try {
    const res = await api.renameFile(oldName, newName.trim(), store.get('currentPath'));
    if (res.ok) { toast('重命名成功', 'success'); fetchFiles(store.get('currentPage')); }
    else { const err = await res.text(); toast(`更名失败: ${err}`, 'error'); }
  } catch (e) { if (e.status !== 401) toast('重命名请求异常', 'error'); }
}

async function moveFileHandler(encodedName) {
  const name = decodeURIComponent(encodedName);
  const targetPath = await promptDialog(`请输入 [${name}] 移动目标路径 (留空: 根目录):`, store.get('currentPath'), '移动文件');
  if (targetPath === null) return;
  try {
    const res = await api.moveFile(name, targetPath.trim(), store.get('currentPath'));
    if (res.ok) { toast('移动成功', 'success'); fetchFiles(store.get('currentPage')); }
    else { const err = await res.text(); toast(`移动失败: ${err}`, 'error'); }
  } catch (e) { if (e.status !== 401) toast('移动请求异常', 'error'); }
}

// ====== 创建文件夹 ======
export async function createFolder() {
  const input = document.getElementById('newFolderName');
  const name = input?.value.trim();
  if (!name) return toast('目录名称不能为空', 'warning');
  try {
    const res = await api.createFolder(name, store.get('currentPath'));
    if (res.ok) { toast(`文件夹 [${name}] 创建成功`, 'success'); input.value = ''; fetchFiles(store.get('currentPage')); }
    else { const err = await res.text(); toast(`新建失败: ${err}`, 'error'); }
  } catch (e) { if (e.status !== 401) toast('新建文件夹失败', 'error'); }
}

// ====== 批量操作 ======
export async function moveSelected() {
  const sel = store.get('selectedFiles');
  if (sel.size === 0) return toast('请先勾选需要批量移动的项目', 'warning');
  const targetPath = await promptDialog(`请输入已选中 (${sel.size}) 个项目要移动到的目标路径 (留空: 根目录):`, store.get('currentPath'), '批量移动');
  if (targetPath === null) return;
  try {
    const res = await api.moveBatch(Array.from(sel), store.get('currentPath'), targetPath.trim());
    if (res.ok) { toast('批量移动成功', 'success'); store.clearSelection(); fetchFiles(store.get('currentPage')); }
    else { const err = await res.text(); toast(`批量移动失败: ${err}`, 'error'); }
  } catch (e) { if (e.status !== 401) toast('批量移动请求异常', 'error'); }
}

export async function downloadSelectedZip() {
  const sel = store.get('selectedFiles');
  if (sel.size === 0) return toast('请先勾选需要打包的文件', 'warning');
  try {
    toast('正在打包远端资源，请稍候...', 'info');
    const res = await api.downloadZip(Array.from(sel), store.get('currentPath'));
    if (res.ok) {
      const blob = await res.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url; a.download = `archive_${Date.now()}.zip`;
      document.body.appendChild(a); a.click(); a.remove();
      URL.revokeObjectURL(url);
      toast('打包下载完毕', 'success');
    } else {
      const err = await res.text();
      toast(`打包失败: ${err}`, 'error');
    }
  } catch (e) { if (e.status !== 401) toast('打包下载请求异常', 'error'); }
}

// ====== 全选 / 选择处理 ======
export function toggleSelectAll(master) {
  const checkboxes = document.querySelectorAll('#file-list-body .item-checkbox');
  if (master.checked) {
    store.selectAll(store.get('files'));
  } else {
    store.clearSelection();
  }
  checkboxes.forEach(cb => { cb.checked = master.checked; });
}

export function handleSelectRow(checkbox) {
  const name = decodeURIComponent(checkbox.dataset.filename);
  if (checkbox.checked) {
    store.get('selectedFiles').add(name);
  } else {
    store.get('selectedFiles').delete(name);
  }
  store._emit('selection:change', store.get('selectedFiles'));
  const checkboxes = document.querySelectorAll('#file-list-body .item-checkbox');
  document.getElementById('selectAllCheckbox').checked =
    checkboxes.length > 0 && Array.from(checkboxes).every(cb => cb.checked);
}

// ====== 分页处理 ======
export function goToPage(page) {
  fetchFiles(page);
}

// ====== 下载 ======
async function downloadFile(url, filename) {
  try {
    const res = await api.request(url);
    if (res.ok) {
      const blob = await res.blob();
      const blobUrl = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = blobUrl; a.download = filename;
      document.body.appendChild(a); a.click(); a.remove();
      URL.revokeObjectURL(blobUrl);
    } else { toast('下载失败', 'error'); }
  } catch (e) { if (e.status !== 401) toast('下载请求异常', 'error'); }
}

// ====== 分享 ======
function openCreateShare(filePath, isDir) {
  const modal = document.getElementById('create-share-modal');
  const pathInput = document.getElementById('share-path');
  const isDirInput = document.getElementById('share-is-dir');
  if (pathInput) pathInput.value = filePath;
  if (isDirInput) isDirInput.value = isDir ? '1' : '0';
  if (modal) { modal.classList.remove('hidden'); modal.style.display = 'flex'; }
}

// ====== 拖拽上传 ======
export function setupDropZone() {
  const zone = document.getElementById('upload-drop-zone');
  if (!zone) return;

  ['dragenter', 'dragover'].forEach(evt => {
    zone.addEventListener(evt, (e) => { e.preventDefault(); zone.classList.add('active'); });
  });
  ['dragleave', 'drop'].forEach(evt => {
    zone.addEventListener(evt, (e) => { e.preventDefault(); zone.classList.remove('active'); });
  });
  zone.addEventListener('drop', (e) => {
    const files = e.dataTransfer?.files;
    if (files?.length) {
      const fileInput = document.getElementById('fileInput');
      if (fileInput) {
        const dt = new DataTransfer();
        dt.items.add(files[0]);
        fileInput.files = dt.files;
        uploadFile();
      }
    }
  });
}
