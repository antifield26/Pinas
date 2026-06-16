// ====== API 客户端 ======
import { API } from './constants.js';

class ApiClient {
  constructor() {
    this.baseUrl = '';
  }

  /** 获取认证头 */
  getAuthHeaders(contentType = null) {
    const token = localStorage.getItem('cloud_auth_token');
    const headers = {};
    if (contentType) headers['Content-Type'] = contentType;
    if (token) headers['Authorization'] = `Bearer ${token}`;
    return headers;
  }

  /** 统一请求方法 */
  async request(url, options = {}, skipAuth = false) {
    const headers = skipAuth ? {} : this.getAuthHeaders();
    if (options.headers) Object.assign(headers, options.headers);

    const controller = new AbortController();
    const timeout = options.timeout || 30000;
    const timer = setTimeout(() => controller.abort(), timeout);

    try {
      const response = await fetch(this.baseUrl + url, {
        ...options,
        headers,
        signal: controller.signal,
      });

      clearTimeout(timer);

      // 401 统一处理：显示登录框
      if (response.status === 401) {
        document.getElementById('login-overlay')?.classList.remove('hidden');
        const err = new Error('Unauthorized');
        err.status = 401;
        throw err;
      }

      return response;
    } catch (err) {
      clearTimeout(timer);
      if (err.name === 'AbortError') {
        const timeoutErr = new Error('请求超时');
        timeoutErr.status = 408;
        console.error(`[API] 请求超时: ${url}`);
        throw timeoutErr;
      }
      if (err.status !== 401) {
        console.error(`[API] 请求失败: ${url}`, err.message || err);
      }
      throw err;
    }
  }

  // ====== 认证 ======
  async login(username, password) {
    return this.request(API.LOGIN, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username, password }),
    }, true);
  }

  async register(username, password) {
    return this.request(API.REGISTER, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username, password }),
    }, true);
  }

  async logout() {
    return this.request(API.LOGOUT, { method: 'POST' });
  }

  // ====== 文件操作 ======
  async listFiles(params = {}) {
    const query = new URLSearchParams();
    if (params.path) query.set('path', params.path);
    if (params.search) query.set('search', params.search);
    if (params.sort_by) query.set('sort_by', params.sort_by);
    if (params.page) query.set('page', params.page);
    if (params.page_size) query.set('page_size', params.page_size);
    return this.request(`${API.FILES_LIST}?${query}`);
  }

  async createFolder(name, currentPath) {
    return this.request(API.FILES_CREATE_FOLDER, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name, current_path: currentPath }),
    });
  }

  async deleteFile(name, currentPath) {
    return this.request(API.FILES_DELETE, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name, current_path: currentPath }),
    });
  }

  async renameFile(name, newName, currentPath) {
    return this.request(API.FILES_RENAME, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name, new_name: newName, current_path: currentPath }),
    });
  }

  async moveFile(name, targetDir, currentPath) {
    return this.request(API.FILES_MOVE, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name, target_dir: targetDir, current_path: currentPath }),
    });
  }

  async moveBatch(names, currentPath, targetPath) {
    return this.request(API.FILES_MOVE_BATCH, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ names, current_path: currentPath, target_path: targetPath }),
    });
  }

  async downloadZip(names, currentPath) {
    return this.request(API.FILES_DOWNLOAD_ZIP, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ names, current_path: currentPath }),
    });
  }

  // ====== 编辑 ======
  async getFileContent(path) {
    return this.request(`${API.EDIT_GET}?path=${encodeURIComponent(path)}`);
  }

  async saveFileContent(path, content) {
    return this.request(API.EDIT_SAVE, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ path, content }),
    });
  }

  // ====== 上传 ======
  async checkChunks(identifier) {
    return this.request(`${API.FILES_CHECK}?identifier=${identifier}`);
  }

  async uploadChunk(formData, identifier, chunkIndex, totalChunks, fileName, parentPath) {
    const params = new URLSearchParams({
      identifier, chunk_index: chunkIndex,
      total_chunks: totalChunks, file_name: fileName,
      parent_path: parentPath || '',
    });
    return this.request(`${API.FILES_UPLOAD_CHUNK}?${params}`, {
      method: 'POST',
      body: formData,
      timeout: 120000,
    });
  }

  async mergeChunks(identifier, fileName, parentPath) {
    return this.request(API.FILES_MERGE, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ identifier, file_name: fileName, parent_path: parentPath }),
    });
  }

  // ====== 分享 ======
  async createShare(filePath, isDir, expireHours, password) {
    return this.request(API.SHARE_CREATE, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ file_path: filePath, is_dir: isDir, expire_hours: expireHours, password }),
    });
  }

  async listShares() {
    return this.request(API.SHARE_LIST);
  }

  async deleteShare(code) {
    return this.request(API.SHARE_DELETE, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ code }),
    });
  }

  // ====== 回收站 ======
  async listTrash() {
    return this.request(API.TRASH_LIST);
  }

  async restoreTrash(id) {
    return this.request(API.TRASH_RESTORE, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ id }),
    });
  }

  async deleteTrashPermanent(id) {
    return this.request(API.TRASH_DELETE, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ id }),
    });
  }

  async clearTrash() {
    return this.request(API.TRASH_CLEAR, { method: 'POST' });
  }

  // ====== 管理 ======
  async getQuota(username) {
    const q = username ? `?username=${encodeURIComponent(username)}` : '';
    return this.request(`${API.ADMIN_QUOTA}${q}`);
  }

  async setQuota(username, quotaMb) {
    return this.request(API.ADMIN_QUOTA, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username, quota_mb: quotaMb }),
    });
  }

  async listUsers() {
    return this.request(API.ADMIN_USERS);
  }

  async resetUserPassword(username, newPassword) {
    return this.request(API.ADMIN_RESET_PASSWORD, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username, new_password: newPassword }),
    });
  }

  async getSystemStatus() {
    return this.request(API.SYSTEM_STATUS);
  }

  // ====== 链接库 ======
  async getLinks() {
    return this.request(API.LINKS);
  }

  async createLink(title, url, icon) {
    return this.request(API.LINKS, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ title, url, icon: icon || null }),
    });
  }

  async updateLink(id, title, url, icon) {
    return this.request(`${API.LINKS}/${id}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ title, url, icon: icon || null }),
    });
  }

  async deleteLink(id) {
    return this.request(`${API.LINKS}/${id}`, { method: 'DELETE' });
  }

  // ====== 待办/日程 ======
  async getTodos(params = {}) {
    const query = new URLSearchParams();
    if (params.category) query.set('category', params.category);
    if (params.status) query.set('status', params.status);
    if (params.priority) query.set('priority', params.priority);
    if (params.search) query.set('search', params.search);
    if (params.date_from) query.set('date_from', params.date_from);
    if (params.date_to) query.set('date_to', params.date_to);
    const qs = query.toString();
    return this.request(`${API.TODOS}${qs ? '?' + qs : ''}`);
  }

  async createTodo(data) {
    return this.request(API.TODOS, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(data),
    });
  }

  async updateTodo(id, data) {
    return this.request(`${API.TODOS}/${id}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(data),
    });
  }

  async deleteTodo(id) {
    return this.request(`${API.TODOS}/${id}`, { method: 'DELETE' });
  }

  // ====== AI Agent ======
  async agentChat(messages, model = null) {
    return this.request(API.AGENT_CHAT, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ messages, model }),
    });
  }

  async agentBriefing(todos, date = null, model = null) {
    return this.request(API.AGENT_BRIEFING, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ todos, date, model }),
    });
  }

  async getAgentModels() {
    return this.request(API.AGENT_MODELS, {}, true);
  }

  // ====== AI Agent 设置 ======
  async getAgentSettings() {
    return this.request(API.AGENT_SETTINGS);
  }

  async saveAgentSettings(settings) {
    return this.request(API.AGENT_SETTINGS, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(settings),
    });
  }
}

// 单例
export const api = new ApiClient();
