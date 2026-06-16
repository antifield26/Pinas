// ====== 全局状态管理 (发布/订阅模式) ======
class Store {
  constructor() {
    this._state = {
      username: localStorage.getItem('cloud_username') || '',
      role: localStorage.getItem('cloud_role') || '',
      authMode: 'login',
      currentPath: '',
      currentView: 'home',
      files: [],
      selectedFiles: new Set(),
      totalFiles: 0,
      totalPages: 0,
      currentPage: 1,
      shares: [],
      links: [],
      isEditorOpen: false,
      editingFile: '',
      isPreviewMode: false,
      systemStatusInterval: null,
      isOnline: navigator.onLine,
      isDark: this._getSystemDarkMode(),
    };
    this._listeners = {};
    this._initDarkMode();
  }

  get(key) { return this._state[key]; }
  set(key, value) {
    this._state[key] = value;
    this._emit(key, value);
  }
  setMultiple(obj) {
    Object.assign(this._state, obj);
    for (const key of Object.keys(obj)) {
      this._emit(key, obj[key]);
    }
  }

  on(event, fn) {
    if (!this._listeners[event]) this._listeners[event] = [];
    this._listeners[event].push(fn);
    return () => {
      this._listeners[event] = this._listeners[event].filter(f => f !== fn);
    };
  }

  _emit(event, data) {
    (this._listeners[event] || []).forEach(fn => {
      try { fn(data); } catch(e) { console.error(`[Store] 事件处理错误 (${event}):`, e); }
    });
  }

  // ====== 认证相关 ======
  isLoggedIn() { return !!this._state.username; }
  isAdmin() { return this._state.role === 'admin'; }

  login(username, role, token) {
    this._state.username = username;
    this._state.role = role;
    localStorage.setItem('cloud_username', username);
    localStorage.setItem('cloud_auth_token', token);
    localStorage.setItem('cloud_role', role);
    this._emit('auth:change', { username, role });
  }

  logout() {
    this._state.username = '';
    this._state.role = '';
    localStorage.removeItem('cloud_auth_token');
    localStorage.removeItem('cloud_username');
    localStorage.removeItem('cloud_role');
    this._state.selectedFiles.clear();
    this._emit('auth:change', { username: '', role: '' });
  }

  // ====== 文件选择 ======
  toggleFileSelect(filename) {
    if (this._state.selectedFiles.has(filename)) {
      this._state.selectedFiles.delete(filename);
    } else {
      this._state.selectedFiles.add(filename);
    }
    this._emit('selection:change', this._state.selectedFiles);
  }
  clearSelection() {
    this._state.selectedFiles.clear();
    this._emit('selection:change', this._state.selectedFiles);
  }
  selectAll(files) {
    this._state.selectedFiles = new Set(files.map(f => f.name));
    this._emit('selection:change', this._state.selectedFiles);
  }

  // ====== 暗色模式 ======
  _getSystemDarkMode() {
    return window.matchMedia?.('(prefers-color-scheme: dark)').matches || false;
  }
  _initDarkMode() {
    this._applyDarkMode(this._state.isDark);
    window.matchMedia?.('(prefers-color-scheme: dark)').addEventListener('change', (e) => {
      if (!localStorage.getItem('cloud_dark_mode')) {
        this.setDarkMode(e.matches);
      }
    });
  }
  setDarkMode(on) {
    this._state.isDark = on;
    localStorage.setItem('cloud_dark_mode', on ? '1' : '0');
    this._applyDarkMode(on);
    this._emit('darkmode:change', on);
  }
  toggleDarkMode() {
    this.setDarkMode(!this._state.isDark);
  }
  _applyDarkMode(on) {
    document.documentElement.classList.toggle('dark', on);
  }
}

// 单例
export const store = new Store();
