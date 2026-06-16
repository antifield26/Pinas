// ====== Toast 通知组件 (单例队列) ======
import { TOAST_DURATION } from '../constants.js';

const COLORS = {
  success: 'bg-green-600',
  error: 'bg-red-600',
  info: 'bg-indigo-600',
  warning: 'bg-amber-500',
};

let container = null;
let currentToast = null;
let hideTimer = null;

function getContainer() {
  if (!container) {
    container = document.getElementById('toast-container');
    if (!container) {
      container = document.createElement('div');
      container.id = 'toast-container';
      container.className = 'fixed top-5 right-5 z-[100] flex flex-col gap-3 pointer-events-none';
      document.body.appendChild(container);
    }
  }
  return container;
}

/**
 * 显示 Toast 通知
 * @param {string} message - 消息文本
 * @param {'success'|'error'|'info'|'warning'} type - 类型
 */
export function toast(message, type = 'info') {
  const ctn = getContainer();

  // 移除旧 toast（单例模式：新 toast 替换旧 toast）
  if (currentToast) {
    currentToast.classList.add('animate-toast-out');
    clearTimeout(hideTimer);
    setTimeout(() => currentToast?.remove(), 300);
  }

  const el = document.createElement('div');
  el.className = `px-4 py-3 rounded-xl shadow-lg text-sm font-medium text-white flex items-center gap-2 pointer-events-auto animate-toast-in`;
  el.classList.add(COLORS[type] || COLORS.info);

  const icons = { success: 'OK', error: 'ERR', info: 'i', warning: '!' };
  el.innerHTML = `<span>${icons[type] || ''}</span> ${message}`;

  ctn.appendChild(el);
  currentToast = el;

  hideTimer = setTimeout(() => {
    if (currentToast === el) {
      el.classList.remove('animate-toast-in');
      el.classList.add('animate-toast-out');
      setTimeout(() => {
        el.remove();
        if (currentToast === el) currentToast = null;
      }, 300);
    }
  }, TOAST_DURATION);
}
