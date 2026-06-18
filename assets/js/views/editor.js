// ====== 在线编辑器视图 ======
import { api } from '../api.js';
import { store } from '../state.js';
import { toast } from '../components/toast.js';
import { fetchFiles } from './drive.js';

export function openEditor(name) {
  // name 已经由 handleFileAction 中的 decodeURIComponent(dataset.filename) 解码
  // 此处不再重复解码，避免双重解码损坏含 % 字符的文件名
  store.set('editingFile', name);
  const titleEl = document.getElementById('editor-title');
  if (titleEl) titleEl.innerText = `编辑器 ⟴ ${name}`;

  const filePath = store.get('currentPath') ? `${store.get('currentPath')}/${name}` : name;
  const textarea = document.getElementById('editor-textarea');
  const preview = document.getElementById('editor-preview');
  const btn = document.getElementById('btn-preview-toggle');

  store.set('isPreviewMode', false);
  if (textarea) textarea.classList.remove('hidden');
  if (preview) preview.classList.add('hidden');
  if (btn) btn.innerText = '预览';

  api.getFileContent(filePath).then(res => {
    if (res.ok) {
      return res.text().then(text => {
        if (textarea) textarea.value = text;
        document.getElementById('editor-modal')?.classList.remove('hidden');
      });
    }
    toast('无法读取文本流内容', 'error');
  }).catch(e => { if (e.status !== 401) toast('读取文件失败', 'error'); });
}

export async function saveFileContent() {
  const filePath = store.get('currentPath') ? `${store.get('currentPath')}/${store.get('editingFile')}` : store.get('editingFile');
  const content = document.getElementById('editor-textarea')?.value;
  if (content === undefined) return;
  try {
    const res = await api.saveFileContent(filePath, content);
    if (res.ok) { toast('保存成功', 'success'); fetchFiles(store.get('currentPage')); }
    else { const err = await res.text(); toast(`保存失败: ${err}`, 'error'); }
  } catch (e) { if (e.status !== 401) toast('保存请求异常', 'error'); }
}

export function toggleEditorMode() {
  const textarea = document.getElementById('editor-textarea');
  const previewEl = document.getElementById('editor-preview');
  const btn = document.getElementById('btn-preview-toggle');
  const isPreview = !store.get('isPreviewMode');
  store.set('isPreviewMode', isPreview);

  if (isPreview) {
    const raw = textarea?.value || '';
    const cleanHtml = window.DOMPurify.sanitize(window.marked.parse(raw));
    if (previewEl) { previewEl.innerHTML = cleanHtml; previewEl.classList.remove('hidden'); }
    if (textarea) textarea.classList.add('hidden');
    if (btn) btn.innerText = '编辑';
  } else {
    if (previewEl) previewEl.classList.add('hidden');
    if (textarea) textarea.classList.remove('hidden');
    if (btn) btn.innerText = '预览';
  }
}

export function closeEditor() {
  document.getElementById('editor-modal')?.classList.add('hidden');
  store.set('editingFile', '');
}
