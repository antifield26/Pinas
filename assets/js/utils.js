// ====== 工具函数 ======
import { SEARCH_DEBOUNCE } from './constants.js';

/**
 * HTML 转义
 */
export function escapeHtml(text) {
  if (!text) return '';
  const map = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#039;' };
  return String(text).replace(/[&<>"']/g, c => map[c]);
}

/**
 * 获取文件扩展名
 */
export function getExtension(filename) {
  return (filename || '').split('.').pop().toLowerCase();
}

/**
 * 防抖函数
 */
export function debounce(fn, delay = SEARCH_DEBOUNCE) {
  let timer;
  return function(...args) {
    clearTimeout(timer);
    timer = setTimeout(() => fn.apply(this, args), delay);
  };
}

/**
 * 复制到剪贴板（优先 Clipboard API）
 */
export async function copyToClipboard(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    // 回退方案
    const textarea = document.createElement('textarea');
    textarea.value = text;
    textarea.style.cssText = 'position:fixed;top:-9999px;left:-9999px';
    document.body.appendChild(textarea);
    textarea.select();
    try {
      document.execCommand('copy');
      return true;
    } catch { return false; }
    finally { document.body.removeChild(textarea); }
  }
}

// 同步版本，避免 await import
export function getFileIconSync(filename, isDir) {
  if (isDir) return '[DIR]';
  const ext = (filename || '').split('.').pop().toLowerCase();
  if (['jpg','jpeg','png','gif','webp','bmp','svg','ico','heic'].includes(ext)) return '[IMG]';
  if (['mp4','webm','ogg','avi','mov','mkv','flv','wmv','3gp'].includes(ext)) return '[VID]';
  if (['mp3','wav','aac','flac','m4a','opus','wma'].includes(ext)) return '[AUD]';
  if (ext === 'pdf') return '[PDF]';
  if (['txt','md','json','js','html','css','rs','py','toml','yml','yaml','xml','log','conf','ini','cfg'].includes(ext)) return '[TXT]';
  if (['zip','rar','7z','tar','gz'].includes(ext)) return '[ZIP]';
  return '[FILE]';
}

/**
 * 检测文件是否为可预览图片
 */
export function isPreviewableImage(filename) {
  return ['jpg','jpeg','png','gif','webp','bmp','svg','ico'].includes(getExtension(filename));
}

/**
 * 检测是否为可预览PDF
 */
export function isPDF(filename) {
  return getExtension(filename) === 'pdf';
}

/**
 * 检测是否为可编辑文本
 */
export function isEditableText(filename) {
  return ['txt','md','markdown','json','ini','conf','rs','toml','js','html','css','yaml','yml','xml','log','cfg','py','rb','go','java','c','cpp','h','sql','env'].includes(getExtension(filename));
}

// ====== SVG 图标（内联复用，避免重复字符串） ======
const SVG = {
  edit: '<svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M11 5H6a2 2 0 00-2 2v11a2 2 0 002 2h11a2 2 0 002-2v-5m-1.414-9.414a2 2 0 112.828 2.828L11.828 15H9v-2.828l8.586-8.586z"/></svg>',
  trash: '<svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M19 7l-.867 12.142A2 2 0 0116.138 21H7.862a2 2 0 01-1.995-1.858L5 7m5 4v6m4-6v6m1-10V4a1 1 0 00-1-1h-4a1 1 0 00-1 1v3M4 7h16"/></svg>',
  check: '<svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M5 13l4 4L19 7"/></svg>',
  play: '<svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M14.752 11.168l-3.197-2.132A1 1 0 0010 9.87v4.263a1 1 0 001.555.832l3.197-2.132a1 1 0 000-1.664z"/><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M21 12a9 9 0 11-18 0 9 9 0 0118 0z"/></svg>',
  calendar: '<svg class="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M8 7V3m8 4V3m-9 8h10M5 21h14a2 2 0 002-2V7a2 2 0 00-2-2H5a2 2 0 00-2 2v12a2 2 0 002 2z"/></svg>',
  plus: '<svg class="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 4v16m8-8H4"/></svg>',
};

/**
 * 获取 SVG 图标 HTML 字符串
 */
export function icon(name) {
  return SVG[name] || '';
}
