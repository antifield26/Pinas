// ====== 自定义模态对话框 ======
import { escapeHtml } from '../utils.js';

/**
 * 显示确认对话框（替代 confirm()）
 */
export function confirmDialog(message, title = '确认操作') {
  return showModal({
    title,
    body: `<p class="text-gray-700 dark:text-gray-300">${escapeHtml(message)}</p>`,
    buttons: [
      { text: '取消', class: 'btn-secondary', value: false },
      { text: '确认', class: 'btn-primary', value: true, autoFocus: true },
    ],
  });
}

/**
 * 显示输入对话框（替代 prompt()）
 */
export function promptDialog(message, defaultValue = '', title = '输入') {
  return showModal({
    title,
    body: `
      <p class="text-sm text-gray-600 dark:text-gray-400 mb-3">${escapeHtml(message)}</p>
      <input type="text" id="modal-input" class="input-field text-base" value="${escapeHtml(defaultValue)}" autofocus>
    `,
    buttons: [
      { text: '取消', class: 'btn-secondary', value: null },
      { text: '确认', class: 'btn-primary', value: 'confirm' },
    ],
    onShown: () => {
      const input = document.getElementById('modal-input');
      if (input) {
        input.focus();
        input.select();
        input.addEventListener('keydown', (e) => {
          if (e.key === 'Enter') {
            e.preventDefault();
            document.getElementById('modal-btn-confirm')?.click();
          }
        });
      }
    },
    getResult: () => document.getElementById('modal-input')?.value || null,
  });
}

/**
 * 显示自定义模态框
 */
export function showModal({
  title,
  body,
  buttons = [{ text: '确定', class: 'btn-primary', value: true }],
  onShown,
  getResult,
  size = 'sm', // sm | md | lg
}) {
  return new Promise((resolve) => {
    // 移除旧模态（如果有）
    const old = document.getElementById('custom-modal');
    if (old) old.remove();

    const sizes = { sm: 'max-w-md', md: 'max-w-lg', lg: 'max-w-2xl' };

    const overlay = document.createElement('div');
    overlay.id = 'custom-modal';
    overlay.className = 'fixed inset-0 bg-black/50 backdrop-blur-sm flex items-center justify-center z-50 p-4 animate-fade-in';
    overlay.innerHTML = `
      <div class="bg-white dark:bg-gray-800 rounded-2xl ${sizes[size]} w-full shadow-2xl animate-slide-up" onclick="event.stopPropagation()">
        <div class="flex justify-between items-center p-5 border-b border-gray-100 dark:border-gray-700">
          <h3 class="text-lg font-semibold text-gray-800 dark:text-gray-100">${escapeHtml(title)}</h3>
          <button class="text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 text-2xl leading-none p-1" id="modal-close-btn">&times;</button>
        </div>
        <div class="p-5">${body}</div>
        <div class="flex justify-end gap-3 p-5 border-t border-gray-100 dark:border-gray-700 bg-gray-50 dark:bg-gray-800/50 rounded-b-2xl">
          ${buttons.map((btn, i) => `
            <button id="modal-btn-${btn.value === 'confirm' ? 'confirm' : i}"
                    class="px-4 py-2 rounded-lg text-sm font-medium transition focus:outline-none focus-visible:ring-2 ${btn.class || 'btn-secondary'}"
                    data-value="${btn.value !== undefined ? btn.value : ''}">
              ${escapeHtml(btn.text)}
            </button>
          `).join('')}
        </div>
      </div>
    `;

    const close = (result) => {
      overlay.classList.add('opacity-0');
      setTimeout(() => overlay.remove(), 200);
      resolve(result);
    };

    // 点击遮罩关闭
    overlay.addEventListener('click', (e) => {
      if (e.target === overlay) close(null);
    });

    // 关闭按钮
    setTimeout(() => {
      document.getElementById('modal-close-btn')?.addEventListener('click', () => close(null));
      // 绑定按钮事件
      buttons.forEach((btn, i) => {
        const id = btn.value === 'confirm' ? 'modal-btn-confirm' : `modal-btn-${i}`;
        const el = document.getElementById(id);
        if (el) {
          el.addEventListener('click', () => {
            if (getResult) {
              const result = getResult();
              if (result === undefined) return; // 验证失败，保持模态框打开
              close(result);
            } else {
              close(btn.value);
            }
          });
          if (btn.autoFocus) el.focus();
        }
      });
      // ESC 关闭
      const escHandler = (e) => {
        if (e.key === 'Escape') {
          close(null);
          document.removeEventListener('keydown', escHandler);
        }
      };
      document.addEventListener('keydown', escHandler);
      if (onShown) onShown();
    }, 50);

    document.body.appendChild(overlay);
  });
}

/**
 * 显示危险操作确认（红色确认按钮）
 */
export function dangerConfirmDialog(message, title = '危险操作') {
  return showModal({
    title,
    body: `<p class="text-gray-700 dark:text-gray-300">${escapeHtml(message)}</p>`,
    buttons: [
      { text: '取消', class: 'btn-secondary', value: false },
      { text: '确认删除', class: 'bg-red-600 hover:bg-red-700 text-white px-4 py-2 rounded-lg text-sm font-medium transition', value: true },
    ],
  });
}
