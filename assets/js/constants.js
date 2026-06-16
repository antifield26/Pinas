// ====== 常量配置 ======
export const API = {
  BASE: '',
  LOGIN: '/api/login',
  REGISTER: '/api/register',
  LOGOUT: '/api/logout',
  FILES_LIST: '/api/files/list',
  FILES_CREATE_FOLDER: '/api/files/create_folder',
  FILES_CHECK: '/api/files/check',
  FILES_UPLOAD_CHUNK: '/api/files/upload_chunk',
  FILES_MERGE: '/api/files/merge',
  FILES_DELETE: '/api/files/delete',
  FILES_RENAME: '/api/files/rename',
  FILES_MOVE: '/api/files/move',
  FILES_MOVE_BATCH: '/api/move_batch',
  FILES_DOWNLOAD_ZIP: '/api/files/download_zip',
  EDIT_GET: '/api/edit/get',
  EDIT_SAVE: '/api/edit/save',
  SYSTEM_STATUS: '/api/system/status',
  SHARE_CREATE: '/api/share/create',
  SHARE_LIST: '/api/share/list',
  SHARE_DELETE: '/api/share/delete',
  TRASH_LIST: '/api/trash/list',
  TRASH_RESTORE: '/api/trash/restore',
  TRASH_DELETE: '/api/trash/delete',
  TRASH_CLEAR: '/api/trash/clear',
  ADMIN_QUOTA: '/api/admin/quota',
  ADMIN_USERS: '/api/admin/users',
  ADMIN_RESET_PASSWORD: '/api/admin/user/reset_password',
  LINKS: '/api/links',
  TODOS: '/api/todos',
  AGENT_CHAT: '/api/agent/chat',
  AGENT_BRIEFING: '/api/agent/briefing',
  AGENT_MODELS: '/api/agent/models',
  AGENT_SETTINGS: '/api/agent/settings',
};

export const CHUNK_SIZE = 5 * 1024 * 1024; // 5 MB
export const TOAST_DURATION = 3500; // ms
export const STATUS_POLL_INTERVAL = 3000; // ms
export const SEARCH_DEBOUNCE = 300; // ms
export const DEFAULT_PAGE_SIZE = 50;
