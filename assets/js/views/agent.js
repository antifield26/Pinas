// ====== AI Agent 视图 — DeepSeek 对话 & 每日简报 ======
import { api } from '../api.js';
import { store } from '../state.js';
import { toast } from '../components/toast.js';
import { showModal, confirmDialog } from '../components/modal.js';
import { escapeHtml } from '../utils.js';

// ====== 对话管理 ======
const STORAGE_KEY = 'cloud_agent_conversations';
const LAST_ACTIVE_KEY = 'cloud_agent_last_conv';

let conversations = [];        // { id, title, messages: [], createdAt, model }
let currentConversationId = null;
let isLoading = false;

// 模型配置
let availableModels = [];
let currentModel = null; // null = 使用服务器默认

// 用户设置缓存
let agentSettings = {
  deepseek_api_key_configured: false,
  deepseek_api_base: null,
  deepseek_model: null,
};

// ====== 对话持久化 ======

function loadConversations() {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) conversations = JSON.parse(raw);
  } catch (e) { conversations = []; }
}

function saveConversations() {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(conversations));
  } catch (e) { /* storage full, ignore */ }
}

function getCurrentConv() {
  if (!currentConversationId) return null;
  return conversations.find(c => c.id === currentConversationId) || null;
}

/** 获取当前对话的消息数组引用 */
function getMessages() {
  const conv = getCurrentConv();
  return conv ? conv.messages : null;
}

/** 自动命名对话：取第一条用户消息的前 20 个字符 */
function autoTitle(conv) {
  if (conv.title !== '新对话') return;
  const firstUser = conv.messages.find(m => m.role === 'user');
  if (firstUser) {
    const t = firstUser.content.replace(/\s+/g, ' ').trim();
    conv.title = t.length > 20 ? t.slice(0, 20) + '…' : t;
    saveConversations();
  }
}

// ====== 对话列表渲染 ======

function renderConversationList() {
  const container = document.getElementById('agent-conv-list');
  if (!container) return;

  if (!conversations.length) {
    container.innerHTML = '';
    return;
  }

  container.innerHTML = conversations.map(c => {
    const isActive = c.id === currentConversationId;
    const msgCount = c.messages.length;
    return `
    <div class="flex items-center gap-1 shrink-0 ${isActive
      ? 'bg-indigo-100 dark:bg-indigo-900/40 text-indigo-700 dark:text-indigo-300 border-indigo-300 dark:border-indigo-600'
      : 'bg-gray-100 dark:bg-gray-700 text-gray-600 dark:text-gray-300 border-gray-200 dark:border-gray-600 hover:bg-gray-200 dark:hover:bg-gray-600'}
      border rounded-lg px-2.5 py-1 cursor-pointer transition group"
      onclick="App.agent.switchConversation('${c.id}')" title="${escapeHtml(c.title)}${msgCount ? ' · ' + msgCount + ' 条消息' : ''}">
      <span class="max-w-[100px] truncate">${escapeHtml(c.title)}</span>
      ${msgCount ? `<span class="text-[10px] opacity-50">${msgCount}</span>` : ''}
      <button onclick="event.stopPropagation();App.agent.deleteConversation('${c.id}')"
        class="ml-0.5 text-xs opacity-0 group-hover:opacity-100 hover:text-red-600 dark:hover:text-red-400 transition shrink-0" title="删除对话">&times;</button>
    </div>`;
  }).join('');
}

// ====== 新建/切换/删除对话 ======

export function newConversation() {
  const id = Date.now().toString(36) + Math.random().toString(36).slice(2, 6);
  const conv = {
    id,
    title: '新对话',
    messages: [],
    createdAt: new Date().toISOString(),
    model: currentModel,
  };
  conversations.unshift(conv);
  currentConversationId = id;
  saveConversations();
  localStorage.setItem(LAST_ACTIVE_KEY, id);
  showWelcome();
  renderConversationList();
  toast('已创建新对话', 'info');
}

export function switchConversation(id) {
  if (id === currentConversationId) return;
  const conv = conversations.find(c => c.id === id);
  if (!conv) return;
  currentConversationId = id;
  localStorage.setItem(LAST_ACTIVE_KEY, id);
  // 同步该对话的模型偏好
  if (conv.model) {
    currentModel = conv.model;
    renderModelSelector();
  }
  const msgs = conv.messages;
  if (msgs.length) {
    renderChat(msgs);
  } else {
    showWelcome();
  }
  renderConversationList();
}

export async function deleteConversation(id) {
  const conv = conversations.find(c => c.id === id);
  if (!conv) return;
  if (conv.messages.length > 0) {
    if (!(await confirmDialog(`确定删除对话"${conv.title}"？此操作不可恢复。`))) return;
  }
  conversations = conversations.filter(c => c.id !== id);
  if (id === currentConversationId) {
    // 切换到第一个对话或创建新对话
    if (conversations.length > 0) {
      switchConversation(conversations[0].id);
    } else {
      currentConversationId = null;
      showWelcome();
    }
  }
  saveConversations();
  renderConversationList();
  toast('对话已删除', 'info');
}

// ====== 上下文精简 ======

export async function compressContext() {
  const conv = getCurrentConv();
  if (!conv || conv.messages.length < 6) {
    toast('对话较短，无需压缩', 'info');
    return;
  }

  const minKeep = 4; // 保留最后 4 条消息（2 轮对话）
  const toSummarize = conv.messages.slice(0, -minKeep);
  const recentOnes = conv.messages.slice(-minKeep);

  // 临时显示压缩中状态
  const oldMessages = conv.messages;
  conv.messages = [
    ...oldMessages,
    { role: 'assistant', content: '⏳ 正在精简上下文...', model: '', usage: null },
  ];
  renderChat(conv.messages);

  try {
    const model = currentModel || document.getElementById('agent-model-select')?.value || null;
    const summaryRequest = [
      ...toSummarize,
      { role: 'user', content: '请将以上所有对话内容精简总结为一段话（保留关键信息：待办事项、用户偏好、重要决策、文件路径等），只输出总结，不要加额外说明。' },
    ];

    const res = await api.agentChat(summaryRequest, model);
    conv.messages = oldMessages; // 恢复

    if (res.ok) {
      const data = await res.json();
      const summary = data.reply || '（总结生成失败）';
      conv.messages = [
        { role: 'assistant', content: `📝 **上下文摘要**: ${summary}`, model: data.model, usage: null },
        ...recentOnes,
      ];
      saveConversations();
      renderChat(conv.messages);
      toast('上下文已精简，释放了大量 token', 'success');
    } else {
      toast('压缩失败，请稍后重试', 'error');
      renderChat(conv.messages);
    }
  } catch (e) {
    conv.messages = oldMessages;
    renderChat(conv.messages);
    toast('压缩请求失败: ' + (e.message || '网络错误'), 'error');
  }
}

// ====== 初始化 ======

export async function initAgentView() {
  // 加载对话历史
  loadConversations();
  const lastId = localStorage.getItem(LAST_ACTIVE_KEY);
  if (lastId && conversations.find(c => c.id === lastId)) {
    currentConversationId = lastId;
  } else if (conversations.length > 0) {
    currentConversationId = conversations[0].id;
  } else {
    // 无历史对话，自动创建第一个
    newConversation();
  }

  // 加载模型列表
  try {
    const res = await api.getAgentModels();
    if (res.ok) availableModels = await res.json();
  } catch (e) { /* ignore */ }

  // 加载用户设置
  await loadSettings();

  // 渲染 UI
  renderModelSelector();
  renderConversationList();

  const conv = getCurrentConv();
  if (conv && conv.messages.length) {
    renderChat(conv.messages);
  } else {
    showWelcome();
  }
}

async function loadSettings() {
  try {
    const res = await api.getAgentSettings();
    if (res.ok) {
      agentSettings = await res.json();
      if (agentSettings.deepseek_model && !currentModel) {
        currentModel = agentSettings.deepseek_model;
      }
    }
  } catch (e) { /* ignore */ }
}

function renderModelSelector() {
  const sel = document.getElementById('agent-model-select');
  if (!sel) return;

  const seen = new Set();
  let options = '<option value="">默认模型</option>';

  if (agentSettings.deepseek_model) {
    seen.add(agentSettings.deepseek_model);
    options += `<option value="${escapeHtml(agentSettings.deepseek_model)}" ${currentModel === agentSettings.deepseek_model ? 'selected' : ''}>${escapeHtml(agentSettings.deepseek_model)} (我的默认)</option>`;
  }

  availableModels.forEach(m => {
    if (!seen.has(m.id)) {
      seen.add(m.id);
      options += `<option value="${m.id}" ${currentModel === m.id ? 'selected' : ''}>${escapeHtml(m.name)}</option>`;
    }
  });

  sel.innerHTML = options;
  updateApiStatusBadge();
}

function updateApiStatusBadge() {
  const badge = document.getElementById('agent-api-status');
  if (!badge) return;
  if (agentSettings.deepseek_api_key_configured) {
    badge.innerHTML = 'API 已配置';
    badge.className = 'text-xs px-2 py-0.5 rounded-full bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-400';
  } else {
    badge.innerHTML = 'API 未配置';
    badge.className = 'text-xs px-2 py-0.5 rounded-full bg-gray-100 text-gray-500 dark:bg-gray-700 dark:text-gray-300';
  }
}

// ====== 欢迎界面 ======

function showWelcome() {
  const container = document.getElementById('agent-chat-messages');
  if (!container) return;
  container.innerHTML = `
    <div class="text-center py-10 space-y-4">
      <div class="text-6xl"></div>
      <h3 class="text-lg font-bold text-gray-700 dark:text-gray-300">AI 智能助手</h3>
      <p class="text-sm text-gray-400 dark:text-gray-400 max-w-md mx-auto">
        由 DeepSeek 驱动。可回答问题、整理待办、生成每日简报。
      </p>
      <div class="flex justify-center gap-3 flex-wrap">
        <button onclick="App.agent.generateBriefing()" class="bg-indigo-600 hover:bg-indigo-700 text-white px-4 py-2 rounded-lg text-sm font-medium transition shadow-sm">
          生成每日简报
        </button>
        <button onclick="App.agent.quickAsk('帮我整理今天的待办事项，按优先级排序')" class="bg-gray-200 hover:bg-gray-300 dark:bg-gray-600 dark:hover:bg-gray-500 text-gray-700 dark:text-gray-200 px-4 py-2 rounded-lg text-sm font-medium transition">
          整理待办
        </button>
      </div>
      <div class="grid grid-cols-1 sm:grid-cols-2 gap-2 max-w-lg mx-auto text-left mt-4">
        ${['今天有什么日程安排？建议我怎么规划时间', '帮我分析文件存储空间的使用情况', '如何提升工作效率？给我一些建议', '写一段 Python 代码：批量重命名文件'].map(q =>
          `<button onclick="App.agent.quickAsk('${q.replace(/'/g, "\\'")}')" class="text-left text-xs text-gray-500 dark:text-gray-400 hover:text-indigo-600 dark:hover:text-indigo-400 bg-gray-50 dark:bg-gray-700/50 rounded-lg px-3 py-2 transition hover:shadow-sm">${q}</button>`
        ).join('')}
      </div>
    </div>
  `;
}

// ====== 消息渲染 ======

function renderChat(msgs) {
  const container = document.getElementById('agent-chat-messages');
  if (!container || !msgs || !msgs.length) return;

  container.innerHTML = msgs.map(m => {
    const isUser = m.role === 'user';
    return `
    <div class="flex gap-3 ${isUser ? 'justify-end' : ''}">
      ${!isUser ? '<div class="text-2xl shrink-0"></div>' : ''}
      <div class="${isUser
        ? 'bg-indigo-600 text-white rounded-2xl rounded-br-md px-4 py-2.5 max-w-[80%] text-sm'
        : 'bg-gray-100 dark:bg-gray-700 rounded-2xl rounded-bl-md px-4 py-2.5 max-w-[80%] text-sm text-gray-800 dark:text-gray-200'}">
        <div class="prose prose-sm dark:prose-invert max-w-none break-words">${isUser ? escapeHtml(m.content) : renderMarkdown(m.content)}</div>
        ${!isUser ? `<div class="text-xs text-gray-400 dark:text-gray-400 mt-1">${m.model || ''} ${m.usage ? '· tokens: ' + m.usage.total_tokens : ''}</div>` : ''}
      </div>
      ${isUser ? '<div class="text-xl shrink-0"></div>' : ''}
    </div>`;
  }).join('');

  setTimeout(() => {
    container.scrollTop = container.scrollHeight;
  }, 100);
}

function renderMarkdown(text) {
  if (!text) return '';
  if (typeof marked !== 'undefined') {
    try {
      const raw = marked.parse(text);
      if (typeof DOMPurify !== 'undefined') {
        return DOMPurify.sanitize(raw, { ALLOWED_TAGS: ['h1','h2','h3','h4','h5','h6','p','br','hr','ul','ol','li','strong','em','del','a','code','pre','blockquote','table','thead','tbody','tr','th','td','img','span','div'], ALLOWED_ATTR: ['href','src','alt','class','target','rel'] });
      }
      return raw;
    } catch (e) { /* fallthrough */ }
  }
  return escapeHtml(text).replace(/\n/g, '<br>');
}

// ====== 发送消息 ======

export async function sendMessage(content) {
  if (isLoading) return;
  if (!content || !content.trim()) return;

  // 确保有活跃对话
  let conv = getCurrentConv();
  if (!conv) {
    newConversation();
    conv = getCurrentConv();
  }

  const input = document.getElementById('agent-input');
  const sendBtn = document.getElementById('agent-send-btn');

  conv.messages.push({ role: 'user', content: content.trim() });
  autoTitle(conv);

  if (conv.messages.length === 1) renderChat(conv.messages);

  conv.messages.push({ role: 'assistant', content: '... 思考中...', model: '', usage: null });
  saveConversations();
  renderChat(conv.messages);
  if (input) input.value = '';

  isLoading = true;
  if (sendBtn) sendBtn.disabled = true;

  try {
    const model = currentModel || document.getElementById('agent-model-select')?.value || null;
    const sendMsgs = conv.messages.filter(m => m.role !== 'assistant' || !m.content.startsWith('...'));
    const res = await api.agentChat(sendMsgs, model);

    conv.messages.pop(); // 移除加载消息

    if (res.ok) {
      const data = await res.json();
      conv.messages.push({
        role: 'assistant',
        content: data.reply || '（AI 未返回内容）',
        model: data.model,
        usage: data.usage,
      });
    } else {
      const err = await res.text();
      conv.messages.push({
        role: 'assistant',
        content: `**错误**: ${escapeHtml(err)}`,
        model: null,
        usage: null,
      });
    }
  } catch (e) {
    conv.messages.pop();
    conv.messages.push({
      role: 'assistant',
      content: `**网络错误**: ${escapeHtml(e.message || '请求失败')}`,
      model: null,
      usage: null,
    });
  }

  isLoading = false;
  if (sendBtn) sendBtn.disabled = false;
  saveConversations();
  renderChat(conv.messages);
  renderConversationList();
  if (input) input.focus();
}

export async function quickAsk(question) {
  const container = document.getElementById('agent-chat-messages');
  if (container && container.querySelector('.text-6xl')) {
    container.innerHTML = '';
  }
  await sendMessage(question);
}

// ====== 每日简报 ======

export async function generateBriefing() {
  if (isLoading) return;

  let conv = getCurrentConv();
  if (!conv) {
    newConversation();
    conv = getCurrentConv();
  }

  // 获取待办数据
  let todos = [];
  try {
    const todosModule = await import('./todos.js');
    todos = await todosModule.getTodosForBriefing();
  } catch (e) {
    try {
      const res = await api.getTodos();
      if (res.ok) todos = await res.json();
    } catch (e2) { /* ignore */ }
  }

  const today = new Date().toISOString().split('T')[0];

  const container = document.getElementById('agent-chat-messages');
  if (container && container.querySelector('.text-6xl')) {
    container.innerHTML = '';
  }

  conv.messages.push({ role: 'user', content: `请为 ${today} 生成每日简报` });
  autoTitle(conv);
  if (conv.messages.length === 1) renderChat(conv.messages);

  conv.messages.push({ role: 'assistant', content: '... 正在生成每日简报...', model: '', usage: null });
  saveConversations();
  renderChat(conv.messages);

  isLoading = true;
  const sendBtn = document.getElementById('agent-send-btn');
  if (sendBtn) sendBtn.disabled = true;

  try {
    const model = currentModel || document.getElementById('agent-model-select')?.value || null;
    const res = await api.agentBriefing(todos, today, model);
    conv.messages.pop();

    if (res.ok) {
      const data = await res.json();
      conv.messages.push({
        role: 'assistant',
        content: data.briefing || '（未能生成简报）',
        model: data.model,
        usage: null,
      });
    } else {
      const err = await res.text();
      conv.messages.push({
        role: 'assistant',
        content: `**简报生成失败**: ${escapeHtml(err)}`,
        model: null,
        usage: null,
      });
    }
  } catch (e) {
    conv.messages.pop();
    conv.messages.push({
      role: 'assistant',
      content: `**网络错误**: ${escapeHtml(e.message || '请求失败')}`,
      model: null,
      usage: null,
    });
  }

  isLoading = false;
  if (sendBtn) sendBtn.disabled = false;
  saveConversations();
  renderChat(conv.messages);
  renderConversationList();
}

// ====== 导出 ======

function downloadBlob(content, filename, mimeType, successMsg) {
  const blob = new Blob([content], { type: mimeType });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
  toast(successMsg, 'success');
}

export function exportConversation() {
  const conv = getCurrentConv();
  if (!conv || !conv.messages.length) { toast('没有可导出的对话', 'warning'); return; }
  const date = new Date().toISOString().split('T')[0];
  let text = `# Antifield Cloud AI 对话记录\n\n**日期**: ${date}\n**模型**: ${currentModel || '默认'}\n**消息数**: ${conv.messages.length}\n\n---\n\n`;
  conv.messages.forEach(m => {
    text += `### ${m.role === 'user' ? '用户' : 'AI'}\n\n${m.content}\n\n`;
    if (m.model) text += `*模型: ${m.model}*\n\n`;
    text += `---\n\n`;
  });
  downloadBlob(text, `ai-conversation-${date}.md`, 'text/markdown;charset=utf-8', '对话已导出为 Markdown 文件');
}

export function exportConversationText() {
  const conv = getCurrentConv();
  if (!conv || !conv.messages.length) { toast('没有可导出的对话', 'warning'); return; }
  const date = new Date().toISOString().split('T')[0];
  let text = `Antifield Cloud AI 对话记录 - ${date}\n\n`;
  conv.messages.forEach(m => {
    text += `[${m.role === 'user' ? '用户' : 'AI'}]\n${m.content}\n\n`;
  });
  downloadBlob(text, `ai-conversation-${date}.txt`, 'text/plain;charset=utf-8', '对话已导出为文本文件');
}

export function clearConversation() {
  const conv = getCurrentConv();
  if (!conv || !conv.messages.length) return;
  conv.messages = [];
  saveConversations();
  const container = document.getElementById('agent-chat-messages');
  if (container) container.innerHTML = '';
  const input = document.getElementById('agent-input');
  if (input) input.value = '';
  showWelcome();
  renderConversationList();
  toast('对话已清空', 'info');
}

// ====== API 设置 ======

export async function openSettings() {
  const hasKey = agentSettings.deepseek_api_key_configured;

  const result = await showModal({
    title: 'AI Agent 设置',
    size: 'md',
    body: `
      <div class="space-y-4">
        <div class="bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800 rounded-lg p-3 text-xs text-amber-700 dark:text-amber-300">
          此处可配置个人 DeepSeek API。不配置则使用服务器全局设置。API Key 加密存储于服务器端。
        </div>
        <div>
          <label class="block text-xs font-semibold text-gray-500 dark:text-gray-300 mb-1">API Key</label>
          <input type="password" id="settings-apikey" class="input-field" placeholder="${hasKey ? '已配置（输入新值覆盖）' : 'sk-...'}">
          ${hasKey ? '<p class="text-xs text-green-600 dark:text-green-400 mt-1">已配置 API Key（已脱敏保存）</p>' : '<p class="text-xs text-gray-400 dark:text-gray-400 mt-1">留空则使用服务器全局设置</p>'}
        </div>
        <div>
          <label class="block text-xs font-semibold text-gray-500 dark:text-gray-300 mb-1">API 基础地址</label>
          <input type="text" id="settings-apibase" class="input-field" placeholder="https://api.deepseek.com" value="${escapeHtml(agentSettings.deepseek_api_base || '')}">
          <p class="text-xs text-gray-400 dark:text-gray-400 mt-1">支持 OpenAI 兼容 API（如 DeepSeek、OpenAI 等）</p>
        </div>
        <div>
          <label class="block text-xs font-semibold text-gray-500 dark:text-gray-300 mb-1">默认模型</label>
          <input type="text" id="settings-model" class="input-field" placeholder="deepseek-v4-flash" value="${escapeHtml(agentSettings.deepseek_model || '')}">
          <p class="text-xs text-gray-400 dark:text-gray-400 mt-1">预设: deepseek-v4-pro[1m] / deepseek-v4-flash</p>
        </div>
      </div>
    `,
    buttons: [
      { text: '取消', class: 'btn-secondary', value: null },
      { text: '保存设置', class: 'btn-primary', value: 'confirm' },
    ],
    getResult: () => {
      const apiKey = document.getElementById('settings-apikey')?.value.trim() || null;
      const apiBase = document.getElementById('settings-apibase')?.value.trim() || null;
      const model = document.getElementById('settings-model')?.value.trim() || null;

      if (apiBase && !apiBase.startsWith('http')) {
        toast('API 基础地址必须以 http:// 或 https:// 开头', 'warning');
        return undefined;
      }

      return { deepseek_api_key: apiKey, deepseek_api_base: apiBase, deepseek_model: model };
    },
  });

  if (result) await saveSettings(result);
}

async function saveSettings(settings) {
  try {
    const payload = {
      deepseek_api_key: settings.deepseek_api_key || null,
      deepseek_api_base: settings.deepseek_api_base || null,
      deepseek_model: settings.deepseek_model || null,
    };

    const res = await api.saveAgentSettings(payload);
    if (res.ok) {
      toast('设置已保存', 'success');
      await loadSettings();
      if (agentSettings.deepseek_model) currentModel = agentSettings.deepseek_model;
      renderModelSelector();
    } else {
      const err = await res.text();
      toast(`保存失败: ${err}`, 'error');
    }
  } catch (e) {
    toast(`保存失败: ${e.message || '网络错误'}`, 'error');
  }
}

export function setModel(modelId) {
  currentModel = modelId || null;
  const conv = getCurrentConv();
  if (conv) {
    conv.model = currentModel;
    saveConversations();
  }
  toast(modelId ? `模型已切换至 ${modelId}` : '已切换至默认模型', 'info');
}

export function handleKeydown(event) {
  if (event.key === 'Enter' && !event.shiftKey) {
    event.preventDefault();
    const input = document.getElementById('agent-input');
    if (input && input.value.trim()) {
      sendMessage(input.value);
    }
  }
}
