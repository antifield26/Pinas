// ====== Antifield Cloud 主应用入口 ======
import { store } from './state.js';
import { api } from './api.js';
import { toast } from './components/toast.js';
// utils.js 函数由各视图模块按需导入

// 视图模块（懒加载）
import * as DriveView from './views/drive.js';
import * as HomeView from './views/home.js';
import * as TrashView from './views/trash.js';
import * as AdminView from './views/admin.js';
import * as ShareView from './views/share.js';
import * as LinksView from './views/links.js';
import * as TodosView from './views/todos.js';
import * as AgentView from './views/agent.js';
import * as UploadView from './views/upload.js';
import * as PlayerView from './views/player.js';
import * as EditorView from './views/editor.js';

// ====== 全局事件委托 ======
function setupGlobalDelegation() {
  // 文件表格事件委托 (click)
  document.getElementById('file-list-body')?.addEventListener('click', async (e) => {
    const target = e.target.closest('[data-action]');
    if (!target) {
      // 检查是否是 checkbox
      const cb = e.target.closest('.item-checkbox');
      if (cb && !e.target.closest('[data-stop]')) {
        DriveView.handleSelectRow(cb);
      }
      return;
    }
    // 阻止行级导航的事件冒泡
    if (e.target.closest('[data-stop]') && target.dataset.action !== 'navigate') {
      e.stopPropagation();
    }
    const { action, ...dataset } = target.dataset;
    await DriveView.handleFileAction(action, dataset);
  });

  // 文件表格行点击导航（仅对目录行和文本文件行）
  document.getElementById('file-list-body')?.addEventListener('click', (e) => {
    const row = e.target.closest('tr[data-action="navigate"]');
    if (row && !e.target.closest('[data-stop]')) {
      DriveView.navigateTo(row.dataset.path);
    }
  });

  // 面包屑导航委托
  document.getElementById('path-breadcrumbs')?.addEventListener('click', (e) => {
    const nav = e.target.closest('[data-nav]');
    if (nav) DriveView.navigateTo(nav.dataset.nav || '');
  });

  // 分页委托
  document.getElementById('pagination-container')?.addEventListener('click', (e) => {
    const btn = e.target.closest('[data-page]');
    if (btn && !btn.disabled) DriveView.goToPage(parseInt(btn.dataset.page));
  });

  // 模态框关闭委托
  document.querySelectorAll('[data-modal-close]').forEach(el => {
    el.addEventListener('click', () => {
      const modal = el.closest('.fixed');
      if (modal) { modal.classList.add('hidden'); modal.style.display = 'none'; }
    });
  });
}

// ====== 视图切换 ======
function switchView(viewName) {
  store.set('currentView', viewName);

  const views = ['view-home', 'view-drive', 'view-trash', 'view-user-admin', 'view-links', 'view-todos', 'view-agent'];
  views.forEach(id => document.getElementById(id)?.classList.add('hidden'));

  switch (viewName) {
    case 'home':
      document.getElementById('view-home')?.classList.remove('hidden');
      updateLayoutByRole();
      break;
    case 'drive':
      document.getElementById('view-drive')?.classList.remove('hidden');
      DriveView.fetchFiles(store.get('currentPage'));
      DriveView.fetchQuota();
      break;
    case 'trash':
      document.getElementById('view-trash')?.classList.remove('hidden');
      TrashView.loadTrashList();
      break;
    case 'user_admin':
      document.getElementById('view-user-admin')?.classList.remove('hidden');
      AdminView.loadUserList();
      break;
    case 'links':
      document.getElementById('view-links')?.classList.remove('hidden');
      LinksView.fetchLinks();
      break;
    case 'todos':
      document.getElementById('view-todos')?.classList.remove('hidden');
      TodosView.loadTodos();
      break;
    case 'agent':
      document.getElementById('view-agent')?.classList.remove('hidden');
      AgentView.initAgentView();
      break;
  }
}

function updateLayoutByRole() {
  const isAdmin = store.isAdmin();
  document.getElementById('system-monitor-card')?.classList.toggle('hidden', !isAdmin);
  document.getElementById('admin-user-card')?.classList.toggle('hidden', !isAdmin);
}

// ====== 认证 ======
async function submitAuth() {
  const username = document.getElementById('login-username')?.value.trim();
  const password = document.getElementById('login-password')?.value;
  const errTip = document.getElementById('login-error');

  if (!username || !password) return toast('请输入用户名和密码', 'warning');

  const isRegister = store.get('authMode') === 'register';
  try {
    const res = isRegister ? await api.register(username, password) : await api.login(username, password);
    if (res.ok) {
      const data = await res.json();
      store.login(username, data.role, data.token);
      document.getElementById('login-overlay')?.classList.add('hidden');
      if (errTip) errTip.classList.add('hidden');
      const pwdInput = document.getElementById('login-password');
      if (pwdInput) pwdInput.value = '';
      toast(isRegister ? '注册成功' : '认证成功，欢迎回来', 'success');
      store.set('authMode', 'login');
      updateLayoutByRole();
      HomeView.startSystemStatusPolling();
      switchView('home');
    } else {
      const errMsg = await res.text();
      if (errTip) { errTip.innerText = `${errMsg || '认证失败'}`; errTip.classList.remove('hidden'); }
    }
  } catch (e) { toast('网关认证请求异常', 'error'); }
}

async function logout() {
  if (!(await (await import('./components/modal.js')).confirmDialog('确定要退出登录吗？'))) return;
  try { await api.logout(); } catch (e) { /* ignore */ }
  store.logout();
  HomeView.stopSystemStatusPolling();
  document.getElementById('login-overlay')?.classList.remove('hidden');
  toast('已安全退出', 'info');
  // 退出后不切换视图 — 登录遮罩已覆盖所有内容，切换视图只会触发不必要的认证请求
}

function toggleAuthMode() {
  const isRegister = store.get('authMode') !== 'register';
  store.set('authMode', isRegister ? 'register' : 'login');
  document.getElementById('login-error')?.classList.add('hidden');
  document.getElementById('auth-title').innerText = isRegister ? '注册账号' : 'Private Cloud';
  document.getElementById('auth-subtitle').innerText = isRegister ? '注册后系统将自动分配独立的网盘空间' : '请输入访问密码';
  document.getElementById('auth-submit-btn').innerText = isRegister ? '立即注册' : '验证并进入';
  document.getElementById('auth-toggle-link').innerText = isRegister ? '已有账号？返回登录' : '没有账号？点击注册';
}

// ====== 键盘快捷键 ======
function setupKeyboardShortcuts() {
  document.addEventListener('keydown', (e) => {
    // 编辑器打开时不触发全局快捷键
    const editorModal = document.getElementById('editor-modal');
    if (editorModal && !editorModal.classList.contains('hidden')) {
      if ((e.ctrlKey || e.metaKey) && e.key === 's') { e.preventDefault(); DriveView.saveFileContent(); }
      if (e.key === 'Escape') DriveView.closeEditor();
      return;
    }
    // 自定义模态框打开时：Esc 关闭
    const customModal = document.getElementById('custom-modal');
    if (customModal && !customModal.classList.contains('hidden')) {
      if (e.key === 'Escape') {
        customModal.dispatchEvent(new Event('click'));
      }
      return;
    }

    const view = store.get('currentView');
    if (view === 'drive') {
      if (e.key === 'Delete') {
        const sel = store.get('selectedFiles');
        if (sel.size > 0) {
          const name = [...sel][0];
          DriveView.handleFileAction('deleteFile', { filename: encodeURIComponent(name) });
        }
      }
      if (e.key === 'F2') {
        const sel = store.get('selectedFiles');
        if (sel.size === 1) {
          DriveView.handleFileAction('renameFile', { filename: encodeURIComponent([...sel][0]) });
        }
      }
      if ((e.ctrlKey || e.metaKey) && e.key === 'a') {
        e.preventDefault();
        document.getElementById('selectAllCheckbox')?.click();
      }
    }
    if (e.key === 'Escape') {
      // 关闭所有模态框
      document.querySelectorAll('.fixed.inset-0.z-50').forEach(m => {
        if (!m.id.includes('custom-modal')) { m.classList.add('hidden'); m.style.display = 'none'; }
      });
      DriveView.closePlayer();
      DriveView.closeEditor();
    }
  });
}

// ====== 暗色模式 ======
function setupDarkModeToggle() {
  const btn = document.getElementById('dark-mode-toggle');
  btn?.addEventListener('click', () => store.toggleDarkMode());
  store.on('darkmode:change', (isDark) => {
    if (btn) btn.innerHTML = isDark ? '亮' : '暗';
  });
}

// ====== 离线检测 ======
function setupOfflineDetection() {
  const banner = document.getElementById('offline-banner');

  window.addEventListener('online', () => {
    store.set('isOnline', true);
    if (banner) banner.classList.add('hidden');
    toast('网络已恢复', 'success');
  });
  window.addEventListener('offline', () => {
    store.set('isOnline', false);
    if (banner) banner.classList.remove('hidden');
    toast('网络连接已断开', 'warning');
  });
}

// ====== PWA: 安装提示 ======
let deferredInstallPrompt = null;

function setupPWAInstallPrompt() {
  window.addEventListener('beforeinstallprompt', (e) => {
    e.preventDefault();
    deferredInstallPrompt = e;
    console.log('[PWA] 安装提示已就绪');
    // 延迟 3 秒后显示自定义安装按钮（避免干扰首次加载）
    setTimeout(() => {
      if (deferredInstallPrompt) showInstallButton();
    }, 3000);
  });

  window.addEventListener('appinstalled', () => {
    console.log('[PWA] 应用已安装');
    deferredInstallPrompt = null;
    const btn = document.getElementById('pwa-install-btn');
    if (btn) btn.remove();
    toast('应用已安装到主屏幕', 'success');
  });

  // 如果已在 standalone 模式，隐藏安装按钮
  if (window.matchMedia('(display-mode: standalone)').matches) {
    console.log('[PWA] 当前运行在 standalone 模式');
    deferredInstallPrompt = null;
  }
}

function showInstallButton() {
  const existing = document.getElementById('pwa-install-btn');
  if (existing) return;

  const btn = document.createElement('button');
  btn.id = 'pwa-install-btn';
  btn.innerHTML = '安装应用';
  btn.title = '将 Antifield Cloud 安装到设备';
  btn.className = 'fixed bottom-20 right-4 z-40 bg-indigo-600 hover:bg-indigo-700 text-white text-sm font-medium px-4 py-2.5 rounded-xl shadow-lg transition-all duration-300 animate-slide-up flex items-center gap-2';
  btn.onclick = async () => {
    if (!deferredInstallPrompt) return;
    deferredInstallPrompt.prompt();
    const { outcome } = await deferredInstallPrompt.userChoice;
    console.log('[PWA] 用户选择:', outcome);
    if (outcome === 'accepted') {
      btn.remove();
    }
    deferredInstallPrompt = null;
  };
  document.body.appendChild(btn);
}

// ====== PWA: 更新通知 ======
function setupPWAUpdateNotification() {
  window.addEventListener('pwa-update-ready', () => {
    const btn = document.createElement('button');
    btn.id = 'pwa-update-btn';
    btn.innerHTML = '新版本已就绪，点击刷新';
    btn.title = '新版本已下载，刷新页面即可更新';
    btn.className = 'fixed bottom-20 right-4 z-40 bg-green-600 hover:bg-green-700 text-white text-sm font-medium px-4 py-2.5 rounded-xl shadow-lg transition-all duration-300 animate-slide-up';
    btn.onclick = () => {
      window.location.reload();
    };
    document.body.appendChild(btn);
    // 10 秒后自动隐藏
    setTimeout(() => { const b = document.getElementById('pwa-update-btn'); if (b) b.remove(); }, 10000);
  });
}

// ====== 用户徽章 ======
store.on('auth:change', ({ username, role }) => {
  const badge = document.getElementById('user-badge');
  if (badge) {
    if (username) {
      badge.innerText = `${username} (${role === 'admin' ? '管理员' : '用户'})`;
      badge.classList.remove('hidden');
    } else {
      badge.classList.add('hidden');
    }
  }
});

// ====== 初始化 ======
console.log('[App] 模块已加载，开始初始化...');

// ====== 公开 API ======
window.App = {
  store,
  api,
  toast,
  // 视图切换
  switchView,
  // 认证
  submitAuth,
  logout,
  toggleAuthMode,
  // Drive
  drive: {
    fetchFiles: (p) => DriveView.fetchFiles(p),
    createFolder: () => DriveView.createFolder(),
    moveSelected: () => DriveView.moveSelected(),
    downloadSelectedZip: () => DriveView.downloadSelectedZip(),
    uploadFile: () => UploadView.uploadFile(),
    toggleSelectAll: (cb) => DriveView.toggleSelectAll(cb),
    handleSearch: () => DriveView.fetchFilesDebounced(),
    handleSort: () => DriveView.fetchFiles(1),
    openEditor: (n) => EditorView.openEditor(n),
    saveFileContent: () => EditorView.saveFileContent(),
    toggleEditorMode: () => EditorView.toggleEditorMode(),
    closeEditor: () => EditorView.closeEditor(),
    closePlayer: () => PlayerView.closePlayer(),
    navigateTo: (p) => DriveView.navigateTo(p),
    refreshList: () => { DriveView.fetchFiles(store.get('currentPage')); toast('列表已刷新'); },
    confirmCreateShare: () => ShareView.confirmCreateShare(),
    closeCreateShareModal: () => ShareView.closeCreateShareModal(),
    openCreateShare: (path, isDir) => {
      const modal = document.getElementById('create-share-modal');
      const pInput = document.getElementById('share-path');
      const dInput = document.getElementById('share-is-dir');
      if (pInput) pInput.value = path;
      if (dInput) dInput.value = isDir ? '1' : '0';
      if (modal) { modal.classList.remove('hidden'); modal.style.display = 'flex'; }
    },
  },
  // 主页（系统监控等功能）
  home: {
    startSystemStatusPolling: () => HomeView.startSystemStatusPolling(),
    stopSystemStatusPolling: () => HomeView.stopSystemStatusPolling(),
  },
  // 链接库
  linksView: {
    fetchLinks: () => LinksView.fetchLinks(),
    addLink: () => LinksView.addLink(),
    editLink: (id) => LinksView.editLink(id),
    deleteLink: (id) => LinksView.deleteLink(id),
  },
  // 分享
  share: {
    openManager: () => ShareView.openShareManager(),
    closeModal: () => ShareView.closeShareModal(),
    copyUrl: (u) => ShareView.copyUrl(u),
    deleteShare: (c) => ShareView.deleteShare(c),
    create: () => ShareView.confirmCreateShare(),
  },
  // 待办/日程
  todos: {
    loadTodos: () => TodosView.loadTodos(),
    addTodo: () => TodosView.addTodo(),
    editTodo: (id) => TodosView.editTodo(id),
    deleteTodo: (id) => TodosView.deleteTodo(id),
    completeTodo: (id) => TodosView.completeTodo(id),
    startTodo: (id) => TodosView.startTodo(id),
    setFilter: (c, s) => TodosView.setFilter(c, s),
    getTodosForBriefing: () => TodosView.getTodosForBriefing(),
    _toggleDatePicker: () => TodosView._toggleDatePicker(),
    _onFormCategoryChange: () => TodosView._onFormCategoryChange(),
    _onTimeTypeChange: () => TodosView._onTimeTypeChange(),
  },
  // AI Agent
  agent: {
    initAgentView: () => AgentView.initAgentView(),
    sendMessage: (c) => AgentView.sendMessage(c),
    quickAsk: (q) => AgentView.quickAsk(q),
    generateBriefing: () => AgentView.generateBriefing(),
    exportConversation: () => AgentView.exportConversation(),
    exportConversationText: () => AgentView.exportConversationText(),
    clearConversation: () => AgentView.clearConversation(),
    compressContext: () => AgentView.compressContext(),
    newConversation: () => AgentView.newConversation(),
    switchConversation: (id) => AgentView.switchConversation(id),
    deleteConversation: (id) => AgentView.deleteConversation(id),
    setModel: (m) => AgentView.setModel(m),
    handleKeydown: (e) => AgentView.handleKeydown(e),
    openSettings: () => AgentView.openSettings(),
  },
  // 回收站
  trash: {
    restore: (id) => TrashView.restore(id),
    permanentDelete: (id) => TrashView.permanentDelete(id),
    clearTrash: () => TrashView.clearTrash(),
  },
  // 管理
  admin: {
    setQuotaPrompt: (u, q) => AdminView.setQuotaPrompt(u, q),
    resetPasswordPrompt: (u) => AdminView.resetPasswordPrompt(u),
  },
};
console.log('[App] window.App 已挂载，共 ' + Object.keys(window.App).length + ' 个顶层 API');

// ====== 页面初始化 ======
// 关键：ES 模块是 deferred 的，此时 DOM 已解析完毕，应立即设置事件处理器。
// 若等待 window.onload（图片等所有资源加载完成），在此之前所有按钮点击都将无效。

window.onpopstate = (event) => {
  const path = event.state?.path || '';
  store.set('currentPath', path);
  store.clearSelection();
  if (store.get('currentView') === 'drive') DriveView.fetchFiles(1);
};

// 使用 try/catch 保护初始化，确保单点错误不会阻断全部功能
try {
  setupGlobalDelegation();
  console.log('[App] ✅ 事件委托已设置');
  setupKeyboardShortcuts();
  console.log('[App] ✅ 键盘快捷键已设置');
  setupDarkModeToggle();
  console.log('[App] ✅ 暗色模式已设置');
  setupOfflineDetection();
  console.log('[App] ✅ 离线检测已设置');
  setupPWAInstallPrompt();
  console.log('[App] ✅ PWA 安装提示已设置');
  setupPWAUpdateNotification();
  console.log('[App] ✅ PWA 更新通知已设置');
  DriveView.setupDropZone();
  console.log('[App] ✅ 拖拽区域已设置');
} catch (e) {
  console.error('[App] 事件绑定失败:', e);
}

// 初始化视图
try {
  const token = localStorage.getItem('cloud_auth_token');
  if (token) {
    console.log('[App] 检测到已登录 token，初始化认证视图...');
    // 登录状态：隐藏登录遮罩，显示对应视图
    document.getElementById('login-overlay')?.classList.add('hidden');
    updateLayoutByRole();
    const urlParams = new URLSearchParams(window.location.search);
    const targetView = urlParams.get('path') ? 'drive' : 'home';
    store.set('currentPath', urlParams.get('path') || '');
    switchView(targetView);
    try { HomeView.startSystemStatusPolling(); } catch (e) { console.error('[App] 系统监控启动失败:', e); }
  } else {
    console.log('[App] 未登录，显示登录遮罩');
  }
  console.log('[App] ✅ 初始化完成');
} catch (e) {
  console.error('[App] 视图初始化失败:', e);
  document.getElementById('login-overlay')?.classList.remove('hidden');
  const loginErr = document.getElementById('login-error');
  if (loginErr) {
    loginErr.innerText = '应用初始化失败: ' + (e.message || '未知错误');
    loginErr.classList.remove('hidden');
  }
}
