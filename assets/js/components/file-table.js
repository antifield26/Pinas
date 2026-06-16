// ====== 文件表格组件（事件委托） ======
import { escapeHtml, getFileIconSync, isPreviewableImage, isPDF, isEditableText } from '../utils.js';
import { store } from '../state.js';

/**
 * 渲染文件表格行（事件委托模式）
 * 所有操作通过 data-* 属性传递，统一在父容器监听 click
 */
export function renderFileTable(files, currentPath, username) {
  const tbody = document.getElementById('file-list-body');
  if (!tbody) return;

  if (!files || files.length === 0) {
    tbody.innerHTML = `<tr><td colspan="4" class="p-8 text-center text-gray-400 dark:text-gray-500">当前目录下空空如也...</td></tr>`;
    document.getElementById('selectAllCheckbox').checked = false;
    return;
  }

  const selected = store.get('selectedFiles');
  const allChecked = files.every(f => selected.has(f.name));

  let html = files.map(item => {
    const safeName = escapeHtml(item.name);
    const encodedName = encodeURIComponent(item.name);
    const fullPath = currentPath ? `${currentPath}/${item.name}` : item.name;
    const pathSegs = fullPath.split('/').map(encodeURIComponent).join('/');
    const downloadPath = username ? `${encodeURIComponent(username)}/${pathSegs}` : pathSegs;
    const fileUrl = `/downloads/${downloadPath}`;
    const isChecked = selected.has(item.name);

    if (item.is_dir) {
      return `
        <tr class="hover:bg-gray-50/80 dark:hover:bg-gray-750 transition cursor-pointer"
            data-action="navigate" data-path="${encodeURIComponent(fullPath)}">
          <td class="p-4" data-stop="true">
            <input type="checkbox" class="item-checkbox rounded text-indigo-600 focus:ring-indigo-500"
                   data-filename="${encodedName}" ${isChecked ? 'checked' : ''}>
          </td>
          <td class="p-4 font-medium text-gray-900 dark:text-gray-100 flex items-center">
            <span class="ml-2">${safeName}</span>
          </td>
          <td class="p-4 text-gray-400 text-xs">-</td>
          <td class="p-4 text-right space-x-2" data-stop="true">
            <button data-action="createShare" data-path="${encodeURIComponent(fullPath)}" data-isdir="1"
                    class="text-xs font-medium text-green-600 hover:text-green-900 transition">分享</button>
            <button data-action="moveFile" data-filename="${encodedName}"
                    class="text-xs font-medium text-indigo-600 hover:text-indigo-900 transition">移动</button>
            <button data-action="renameFile" data-filename="${encodedName}"
                    class="text-xs font-medium text-amber-600 hover:text-amber-900 transition">重命名</button>
            <button data-action="deleteFile" data-filename="${encodedName}"
                    class="text-xs font-medium text-red-600 hover:text-red-900 transition">删除</button>
          </td>
        </tr>`;
    } else {
      const ext = (item.name || '').split('.').pop().toLowerCase();
      const icon = getFileIconSync(item.name, false);
      const canPreview = isPreviewableImage(item.name) || isPDF(item.name);
      const canEdit = isEditableText(item.name);
      const canPlay = ['mp4','webm','ogg','avi','mov','mkv','flv','wmv'].includes(ext) ? 'video'
        : ['mp3','wav','aac','flac','m4a','opus'].includes(ext) ? 'audio' : null;
      const isTextFile = ['txt','md','json','js','html','css','rs','py','toml','yml','yaml','xml','log','ini','cfg','conf'].includes(ext);

      // 预览/播放需要完整路径（currentPath + filename）
      const encodedFullPath = encodeURIComponent(fullPath);

      return `
        <tr class="hover:bg-gray-50/80 dark:hover:bg-gray-750 transition">
          <td class="p-4">
            <input type="checkbox" class="item-checkbox rounded text-indigo-600 focus:ring-indigo-500"
                   data-filename="${encodedName}" ${isChecked ? 'checked' : ''}>
          </td>
          <td class="p-4 font-medium text-gray-800 dark:text-gray-200 max-w-xs truncate ${isTextFile ? 'cursor-pointer' : ''}"
              title="${safeName}" ${isTextFile ? `data-action="openEditor" data-filename="${encodedName}"` : ''}>
            ${icon} ${safeName}
          </td>
          <td class="p-4 text-gray-500 dark:text-gray-400 text-xs">${item.size_mb} MB</td>
          <td class="p-4 text-right space-x-2">
            ${canPreview ? `<button data-action="previewFile" data-path="${encodedFullPath}" data-filename="${encodedName}" data-type="${ext === 'pdf' ? 'pdf' : 'image'}"
              class="text-xs font-medium text-indigo-600 hover:text-indigo-900 transition">预览</button>` : ''}
            ${canPlay ? `<button data-action="playMedia" data-path="${encodedFullPath}" data-filename="${encodedName}" data-type="${canPlay}"
              class="text-xs font-medium text-${canPlay === 'video' ? 'green' : 'purple'}-600 hover:text-${canPlay === 'video' ? 'green' : 'purple'}-900 transition">▶️</button>` : ''}
            <button data-action="createShare" data-path="${encodedFullPath}" data-isdir="0"
              class="text-xs font-medium text-green-600 hover:text-green-900 transition">分享</button>
            ${canEdit ? `<button data-action="openEditor" data-filename="${encodedName}"
              class="text-xs font-medium text-indigo-600 hover:text-indigo-900 transition">编辑</button>` : ''}
            <button data-action="downloadFile" data-url="${fileUrl}" data-filename="${safeName}"
              class="text-xs font-medium text-blue-600 hover:text-blue-900 transition">下载</button>
            <button data-action="moveFile" data-filename="${encodedName}"
              class="text-xs font-medium text-indigo-600 hover:text-indigo-900 transition">移动</button>
            <button data-action="renameFile" data-filename="${encodedName}"
              class="text-xs font-medium text-amber-600 hover:text-amber-900 transition">重命名</button>
            <button data-action="deleteFile" data-filename="${encodedName}"
              class="text-xs font-medium text-red-600 hover:text-red-900 transition">删除</button>
          </td>
        </tr>`;
    }
  }).join('');

  tbody.innerHTML = html;
  document.getElementById('selectAllCheckbox').checked = allChecked;
}

/**
 * 渲染分页控件
 */
export function renderPagination(page, totalPages, total) {
  const container = document.getElementById('pagination-container');
  if (!container) return;

  if (totalPages <= 1) {
    container.innerHTML = '';
    return;
  }

  let html = `<div class="flex items-center justify-between pt-2 text-xs text-gray-500 dark:text-gray-400">
    <span>共 ${total} 项</span>
    <div class="flex items-center gap-1">`;

  html += `<button data-page="${page - 1}" class="px-2 py-1 rounded hover:bg-gray-100 dark:hover:bg-gray-700 disabled:opacity-30" ${page <= 1 ? 'disabled' : ''}>‹</button>`;

  for (let i = 1; i <= totalPages; i++) {
    if (i === 1 || i === totalPages || (i >= page - 1 && i <= page + 1)) {
      html += `<button data-page="${i}" class="px-2 py-1 rounded ${i === page ? 'bg-indigo-600 text-white' : 'hover:bg-gray-100 dark:hover:bg-gray-700'}">${i}</button>`;
    } else if (i === page - 2 || i === page + 2) {
      html += `<span class="px-1">…</span>`;
    }
  }

  html += `<button data-page="${page + 1}" class="px-2 py-1 rounded hover:bg-gray-100 dark:hover:bg-gray-700 disabled:opacity-30" ${page >= totalPages ? 'disabled' : ''}>›</button>`;
  html += `</div></div>`;

  container.innerHTML = html;
}
