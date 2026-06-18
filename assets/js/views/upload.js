// ====== 分片上传视图 ======
import { api } from '../api.js';
import { store } from '../state.js';
import { toast } from '../components/toast.js';
import { CHUNK_SIZE } from '../constants.js';
import { fetchFiles } from './drive.js';

let currentUploadAbort = null;

export async function uploadFile() {
  const fileInput = document.getElementById('fileInput');
  if (!fileInput?.files.length) return toast('请先挂载待上传的文件', 'warning');
  const file = fileInput.files[0];

  const identifier = btoa(encodeURIComponent(file.name)) + `_${file.size}_${file.lastModified}`;
  const totalChunks = Math.ceil(file.size / CHUNK_SIZE);

  const pContainer = document.getElementById('progressContainer');
  const pStatus = document.getElementById('uploadStatus');
  const pText = document.getElementById('progressText');
  const pBar = document.getElementById('progressBar');

  const updateUI = (index, total) => {
    const pct = Math.round((index / total) * 100);
    if (pText) pText.innerText = `${pct}%`;
    if (pBar) pBar.style.width = `${pct}%`;
  };

  if (pContainer) pContainer.classList.remove('hidden');
  if (pStatus) pStatus.innerText = '正在验证断点续传状态...';
  updateUI(0, totalChunks);

  // 检查已上传分片
  let uploadedChunks = [];
  try {
    const checkRes = await api.checkChunks(identifier);
    if (checkRes.ok) {
      const checkData = await checkRes.json();
      uploadedChunks = checkData.uploaded_chunks || [];
    }
  } catch (err) {
    if (err.status === 401) { if (pContainer) pContainer.classList.add('hidden'); return; }
    if (pContainer) pContainer.classList.add('hidden');
    return toast('秒传校验链路故障', 'error');
  }

  // 上传每个分片
  for (let i = 0; i < totalChunks; i++) {
    if (uploadedChunks.includes(i)) { updateUI(i + 1, totalChunks); continue; }

    if (pStatus) pStatus.innerText = `正在传输第 ${i + 1}/${totalChunks} 块分片...`;
    const start = i * CHUNK_SIZE;
    const end = Math.min(file.size, start + CHUNK_SIZE);
    const blob = file.slice(start, end);
    const formData = new FormData();
    formData.append('file', blob);

    try {
      const res = await api.uploadChunk(formData, identifier, i, totalChunks, file.name, store.get('currentPath'));
      if (res.status === 401) {
        document.getElementById('login-overlay')?.classList.remove('hidden');
        if (pContainer) pContainer.classList.add('hidden');
        return;
      }
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      updateUI(i + 1, totalChunks);
    } catch (err) {
      if (err.status === 401) { if (pContainer) pContainer.classList.add('hidden'); return; }
      if (pStatus) pStatus.innerText = '传输中断';
      return toast(`切片 ${i + 1} 上传失败，请重试`, 'error');
    }
  }

  // 合并
  if (pStatus) pStatus.innerText = '全部分片发送完毕，正在整合落盘...';
  try {
    const mergeRes = await api.mergeChunks(identifier, file.name, store.get('currentPath'));
    if (mergeRes.ok) {
      toast('上传成功', 'success');
      fileInput.value = '';
      fetchFiles(store.get('currentPage'));
      setTimeout(() => pContainer?.classList.add('hidden'), 2000);
    } else {
      const errMsg = await mergeRes.text();
      toast(`合并失败: ${errMsg}`, 'error');
      if (pStatus) pStatus.innerText = '落盘终止';
    }
  } catch (e) {
    if (e.status === 401) { if (pContainer) pContainer.classList.add('hidden'); return; }
    if (pStatus) pStatus.innerText = '网关响应错误';
  }
}
