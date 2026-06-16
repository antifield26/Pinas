// ====== 待办事项 / 日程视图 ======
import { api } from '../api.js';
import { store } from '../state.js';
import { toast } from '../components/toast.js';
import { confirmDialog, promptDialog, showModal } from '../components/modal.js';
import { escapeHtml, icon } from '../utils.js';

// 当前过滤状态
let currentCategory = 'all';   // all | todo | schedule
let currentStatus = 'pending'; // pending | in_progress | completed | expired | all

const PRIORITY = {
  high:   { label: '高', cls: 'bg-red-100 text-red-700 dark:bg-red-900/30 dark:text-red-400' },
  medium: { label: '中', cls: 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400' },
  low:    { label: '低', cls: 'bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-400' },
};

const STATUS = {
  pending:     { label: '待办',   cls: 'bg-gray-100 text-gray-600 dark:bg-gray-700 dark:text-gray-400' },
  in_progress: { label: '进行中', cls: 'bg-blue-100 text-blue-700 dark:bg-blue-900/30 dark:text-blue-400' },
  completed:   { label: '已完成', cls: 'bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-400 line-through' },
  expired:     { label: '已过期', cls: 'bg-red-100 text-red-700 dark:bg-red-900/30 dark:text-red-400' },
};

const CAT = { todo: '待办', schedule: '日程' };

/**
 * 检测是否已过期（到期日已过且未完成）
 */
function isExpired(todo) {
  if (todo.status === 'completed') return false;
  if (!todo.due_date) return false;
  const due = new Date(todo.due_date);
  const now = new Date();
  return due < new Date(now.getFullYear(), now.getMonth(), now.getDate());
}

/** 获取显示状态 */
function displayStatus(t) {
  // 日程状态由后端自动计算，直接使用
  if (t.status === 'expired') return STATUS.expired;
  if (t.status === 'completed') return STATUS.completed;
  if (t.status === 'in_progress') return STATUS.in_progress;
  if (isExpired(t)) return STATUS.expired;
  return STATUS[t.status] || STATUS.pending;
}

export async function loadTodos() {
  if (!store.isLoggedIn()) return;
  try {
    const params = {};
    if (currentCategory !== 'all') params.category = currentCategory;
    if (currentStatus !== 'all') params.status = currentStatus;
    const res = await api.getTodos(params);
    if (res.ok) {
      const todos = await res.json();
      renderTodos(todos);
    }
  } catch (e) { console.error('[Todos] 加载待办失败', e); }
}

function renderTodos(todos) {
  const container = document.getElementById('todos-container');
  const countEl = document.getElementById('todos-count');
  if (!container) return;

  if (countEl) countEl.innerText = `共 ${todos.length} 项`;

  if (!todos.length) {
    const label = currentCategory === 'schedule' ? '日程' : currentCategory === 'todo' ? '待办' : '待办/日程';
    container.innerHTML = `<div class="text-center text-gray-400 dark:text-gray-400 py-10">暂无${label}，点击"添加"创建</div>`;
    return;
  }

  const scheduleItems = todos.filter(t => t.category === 'schedule');
  const todoItems = todos.filter(t => t.category !== 'schedule');

  let html = '';

  if (currentCategory === 'all' || currentCategory === 'schedule') {
    html += renderGroup('schedule', '日程安排', scheduleItems);
  }
  if (currentCategory === 'all' || currentCategory === 'todo') {
    html += renderGroup('todo', '待办事项', todoItems);
  }

  container.innerHTML = html || '<div class="text-center text-gray-400 dark:text-gray-400 py-10 col-span-full">暂无匹配项</div>';
}

function renderGroup(cat, title, items) {
  if (!items.length) return '';
  return `
    <div class="mb-6">
      <h3 class="text-sm font-semibold text-gray-500 dark:text-gray-400 uppercase tracking-wider mb-3">
        ${title} <span class="text-xs font-normal normal-case">(${items.length})</span>
      </h3>
      <div class="grid gap-3 sm:grid-cols-2">
        ${items.map(t => renderTodoCard(t)).join('')}
      </div>
    </div>`;
}

function formatTimeRange(t) {
  if (t.category !== 'schedule' || !t.due_date) return '';
  const dueDate = t.due_date.substring(0, 10);

  if (t.is_all_day === 1 || t.is_all_day === true) {
    return `<span class="text-xs text-indigo-500 dark:text-indigo-400 font-medium">${escapeHtml(dueDate)} 全天</span>`;
  }

  const start = t.start_time || '00:00';
  const end = t.end_time || '23:59';
  return `<span class="text-xs text-indigo-500 dark:text-indigo-400 font-medium">${escapeHtml(dueDate)} ${escapeHtml(start)}–${escapeHtml(end)}</span>`;
}

function formatTodoDueDate(dueDate) {
  if (!dueDate) return '';
  // 包含时间部分 (ISO 格式 YYYY-MM-DDTHH:MM:SS)
  if (dueDate.includes('T')) {
    const [date, time] = dueDate.split('T');
    return `${date} ${time.substring(0, 5)}`;
  }
  return dueDate.substring(0, 10);
}

function renderTodoCard(t) {
  const p = PRIORITY[t.priority] || PRIORITY.medium;
  const s = displayStatus(t);
  const isSchedule = t.category === 'schedule';
  const isDone = t.status === 'completed';

  const desc = t.description
    ? `<p class="text-xs text-gray-500 dark:text-gray-300 mt-1 line-clamp-2">${escapeHtml(t.description)}</p>`
    : '';

  // 日程显示时间段，待办显示截止日期
  const dueHtml = isSchedule
    ? formatTimeRange(t)
    : (t.due_date
        ? `<span class="text-xs ${t.status === 'expired' || isExpired(t) ? 'text-red-500 dark:text-red-400 font-medium' : 'text-gray-400 dark:text-gray-400'}">${escapeHtml(formatTodoDueDate(t.due_date))}</span>`
        : '');

  const catLabel = CAT[t.category] || '待办';

  // 日程不显示手动操作按钮（状态自动管理）
  const actionButtons = isSchedule
    ? `
        <button onclick="App.todos.editTodo(${t.id})" class="w-8 h-8 flex items-center justify-center rounded-lg text-amber-600 bg-gray-100 dark:bg-gray-700 hover:bg-amber-50 dark:hover:bg-amber-900/40 transition" title="编辑">
          ${icon('edit')}
        </button>
        <button onclick="App.todos.deleteTodo(${t.id})" class="w-8 h-8 flex items-center justify-center rounded-lg text-red-600 bg-gray-100 dark:bg-gray-700 hover:bg-red-50 dark:hover:bg-red-900/40 transition" title="删除">
          ${icon('trash')}
        </button>`
    : `
        ${!isDone ? `
        <button onclick="App.todos.completeTodo(${t.id})" class="w-8 h-8 flex items-center justify-center rounded-lg text-green-600 bg-gray-100 dark:bg-gray-700 hover:bg-green-50 dark:hover:bg-green-900/40 transition" title="标记完成">
          ${icon('check')}
        </button>
        ` : ''}
        ${t.status !== 'in_progress' && !isDone ? `
        <button onclick="App.todos.startTodo(${t.id})" class="w-8 h-8 flex items-center justify-center rounded-lg text-blue-600 bg-gray-100 dark:bg-gray-700 hover:bg-blue-50 dark:hover:bg-blue-900/40 transition" title="开始进行">
          ${icon('play')}
        </button>
        ` : ''}
        <button onclick="App.todos.editTodo(${t.id})" class="w-8 h-8 flex items-center justify-center rounded-lg text-amber-600 bg-gray-100 dark:bg-gray-700 hover:bg-amber-50 dark:hover:bg-amber-900/40 transition" title="编辑">
          ${icon('edit')}
        </button>
        <button onclick="App.todos.deleteTodo(${t.id})" class="w-8 h-8 flex items-center justify-center rounded-lg text-red-600 bg-gray-100 dark:bg-gray-700 hover:bg-red-50 dark:hover:bg-red-900/40 transition" title="删除">
          ${icon('trash')}
        </button>`;

  return `
    <div class="bg-white dark:bg-gray-800 border border-gray-100 dark:border-gray-700 rounded-xl p-4 hover:shadow-md transition group ${isDone ? 'opacity-60' : ''}">
      <div class="flex items-start justify-between gap-3">
        <div class="flex-1 min-w-0">
          <div class="flex items-center gap-2 flex-wrap mb-1">
            <span class="text-xs px-2 py-0.5 rounded-full font-medium ${s.cls}">${s.label}</span>
            <span class="text-xs px-2 py-0.5 rounded-full font-medium ${p.cls}">${p.label}</span>
            <span class="text-xs text-gray-400 dark:text-gray-500">${catLabel}</span>
          </div>
          <h3 class="font-medium text-gray-800 dark:text-gray-200 ${isDone ? 'line-through' : ''}">${escapeHtml(t.title)}</h3>
          ${desc}
          <div class="flex items-center gap-3 mt-2 flex-wrap">${dueHtml}</div>
        </div>
        <div class="flex items-center gap-0.5 shrink-0 transition">
          ${actionButtons}
        </div>
      </div>
    </div>`;
}

/** 共享待办/日程表单（新建/编辑复用） */
async function renderTodoForm(todo = null) {
  const isEdit = todo !== null;
  const isSchedule = todo?.category === 'schedule';
  const isAllDay = todo?.is_all_day === 1 || todo?.is_all_day === true || (!isEdit);

  return showModal({
    title: isEdit ? `编辑${isSchedule ? '日程' : '待办'}` : `新建${isSchedule ? '日程' : '待办'}`,
    size: 'md',
    body: `
      <div class="space-y-3">
        <div>
          <label class="block text-xs font-semibold text-gray-500 dark:text-gray-300 mb-1">类型</label>
          <select id="todo-category" class="input-field" onchange="App.todos._onFormCategoryChange()">
            <option value="todo" ${todo?.category === 'todo' || !isSchedule ? 'selected' : ''}>待办事项</option>
            <option value="schedule" ${isSchedule ? 'selected' : ''}>日程安排</option>
          </select>
        </div>
        <div>
          <label class="block text-xs font-semibold text-gray-500 dark:text-gray-300 mb-1">标题 *</label>
          <input type="text" id="todo-title" class="input-field" value="${escapeHtml(todo?.title || '')}" placeholder="${isEdit ? '' : '输入标题...'}">
        </div>
        <div>
          <label class="block text-xs font-semibold text-gray-500 dark:text-gray-300 mb-1">描述</label>
          <textarea id="todo-desc" class="input-field" rows="2" placeholder="${isEdit ? '' : '可选描述...'}">${escapeHtml(todo?.description || '')}</textarea>
        </div>

        <!-- 日程日期时间区块 -->
        <div id="todo-schedule-block" class="${isSchedule ? '' : 'hidden'} space-y-3 border border-indigo-200 dark:border-indigo-700 rounded-lg p-3 bg-indigo-50/50 dark:bg-indigo-900/10">
          <div>
            <label class="block text-xs font-semibold text-gray-500 dark:text-gray-300 mb-1">日期 *</label>
            <input type="date" id="todo-due" class="input-field" value="${todo?.due_date ? todo.due_date.substring(0, 10) : ''}" placeholder="年/月/日" required>
          </div>
          <div>
            <label class="block text-xs font-semibold text-gray-500 dark:text-gray-300 mb-1">时间类型</label>
            <select id="todo-time-type" class="input-field" onchange="App.todos._onTimeTypeChange()">
              <option value="all_day" ${isAllDay ? 'selected' : ''}>全天</option>
              <option value="custom" ${!isAllDay ? 'selected' : ''}>自定义时间段</option>
            </select>
          </div>
          <div id="todo-time-range" class="grid grid-cols-2 gap-2 ${isAllDay ? 'hidden' : ''}">
            <div>
              <label class="block text-xs text-gray-400 dark:text-gray-500 mb-1">开始时间</label>
              <input type="time" id="todo-start-time" class="input-field text-sm" value="${escapeHtml(todo?.start_time || '')}">
            </div>
            <div>
              <label class="block text-xs text-gray-400 dark:text-gray-500 mb-1">结束时间</label>
              <input type="time" id="todo-end-time" class="input-field text-sm" value="${escapeHtml(todo?.end_time || '')}">
            </div>
          </div>
        </div>

        <!-- 待办日期选择（可选，折叠式） -->
        <div id="todo-date-block" class="${isSchedule ? 'hidden' : ''}">
          <button type="button" id="todo-date-toggle" class="flex items-center gap-2 text-sm text-gray-500 dark:text-gray-400 hover:text-indigo-600 dark:hover:text-indigo-400 transition p-2 rounded-lg border border-dashed border-gray-300 dark:border-gray-600 w-full justify-center"
                  onclick="App.todos._toggleDatePicker()">
            ${icon('calendar')}
            <span id="todo-date-label">${todo?.due_date ? escapeHtml(formatTodoDueDate(todo.due_date)) : '设置截止日期（可选）'}</span>
          </button>
          <div id="todo-date-picker" class="${todo?.due_date ? '' : 'hidden'} mt-2 p-3 bg-gray-50 dark:bg-gray-700/50 rounded-lg space-y-2">
            <div class="grid grid-cols-2 gap-2">
              <input type="date" id="todo-todo-due" class="input-field text-sm" value="${todo?.due_date ? todo.due_date.substring(0, 10) : ''}">
              <input type="time" id="todo-todo-time" class="input-field text-sm" value="${todo?.due_date ? todo.due_date.substring(11, 16) || '' : ''}">
            </div>
          </div>
        </div>

        <div class="grid ${isEdit && !isSchedule ? 'grid-cols-3' : 'grid-cols-2'} gap-3">
          <div>
            <label class="block text-xs font-semibold text-gray-500 dark:text-gray-300 mb-1">优先级</label>
            <select id="todo-priority" class="input-field">
              <option value="low" ${todo?.priority === 'low' ? 'selected' : ''}>低</option>
              <option value="medium" ${todo?.priority === 'medium' || !isEdit ? 'selected' : ''}>中</option>
              <option value="high" ${todo?.priority === 'high' ? 'selected' : ''}>高</option>
            </select>
          </div>
          ${isEdit && !isSchedule ? `
          <div>
            <label class="block text-xs font-semibold text-gray-500 dark:text-gray-300 mb-1">状态</label>
            <select id="todo-status" class="input-field">
              <option value="pending" ${todo?.status === 'pending' ? 'selected' : ''}>待办</option>
              <option value="in_progress" ${todo?.status === 'in_progress' ? 'selected' : ''}>进行中</option>
              <option value="completed" ${todo?.status === 'completed' ? 'selected' : ''}>已完成</option>
              <option value="expired" ${todo?.status === 'expired' ? 'selected' : ''}>已过期</option>
            </select>
          </div>` : ''}
        </div>
      </div>
    `,
    buttons: [
      { text: '取消', class: 'btn-secondary', value: null },
      { text: isEdit ? '保存' : '创建', class: 'btn-primary', value: 'confirm' },
    ],
    onShown: () => {
      const titleInput = document.getElementById('todo-title');
      if (titleInput) titleInput.focus();
    },
    getResult: () => {
      const title = document.getElementById('todo-title')?.value.trim();
      if (!title) { toast('请输入标题', 'warning'); return undefined; }

      const category = document.getElementById('todo-category')?.value || 'todo';
      const isSched = category === 'schedule';

      let dueDateStr = null;
      let isAllDay = true;
      let startTime = null;
      let endTime = null;

      if (isSched) {
        // 日程：日期必填
        const dueDate = document.getElementById('todo-due')?.value;
        if (!dueDate) { toast('日程必须设置日期', 'warning'); return undefined; }
        dueDateStr = dueDate;

        const timeType = document.getElementById('todo-time-type')?.value;
        isAllDay = timeType !== 'custom';

        if (!isAllDay) {
          startTime = document.getElementById('todo-start-time')?.value || null;
          endTime = document.getElementById('todo-end-time')?.value || null;
          if (!startTime || !endTime) {
            toast('请设置开始和结束时间', 'warning');
            return undefined;
          }

          // 合并日期和时间
          dueDateStr = `${dueDate}T${startTime}:00`;
        }
      } else {
        // 待办：日期可选，支持精确到分钟
        const dueDate = document.getElementById('todo-todo-due')?.value;
        if (dueDate) {
          const dueTime = document.getElementById('todo-todo-time')?.value;
          dueDateStr = dueTime ? `${dueDate}T${dueTime}:00` : dueDate;
        } else {
          dueDateStr = null;
        }
      }

      const status = (isEdit && !isSched)
        ? document.getElementById('todo-status')?.value
        : 'pending';

      return {
        title,
        description: document.getElementById('todo-desc')?.value.trim() || null,
        category,
        priority: document.getElementById('todo-priority')?.value,
        status,
        due_date: dueDateStr,
        is_all_day: isAllDay,
        start_time: startTime,
        end_time: endTime,
      };
    },
  });
}

/** 切换待办日期选择器可见性（仅待办模式使用） */
export function _toggleDatePicker() {
  const picker = document.getElementById('todo-date-picker');
  const label = document.getElementById('todo-date-label');
  const toggle = document.getElementById('todo-date-toggle');
  if (!picker || !label || !toggle) return;
  const isHidden = picker.classList.contains('hidden');
  if (isHidden) {
    picker.classList.remove('hidden');
    label.textContent = '截止日期（可选）';
    toggle.classList.remove('border-dashed');
    toggle.classList.add('border-indigo-400', 'dark:border-indigo-500');
    document.getElementById('todo-todo-due')?.focus();
  } else {
    picker.classList.add('hidden');
    label.textContent = '设置截止日期（可选）';
    toggle.classList.add('border-dashed');
    toggle.classList.remove('border-indigo-400', 'dark:border-indigo-500');
    const dueInput = document.getElementById('todo-todo-due');
    if (dueInput) dueInput.value = '';
    const timeInput = document.getElementById('todo-todo-time');
    if (timeInput) timeInput.value = '';
  }
}

/** 切换日程时间类型（全天 / 自定义时间段） */
export function _onTimeTypeChange() {
  const timeType = document.getElementById('todo-time-type')?.value;
  const timeRange = document.getElementById('todo-time-range');
  if (!timeRange) return;
  if (timeType === 'custom') {
    timeRange.classList.remove('hidden');
  } else {
    timeRange.classList.add('hidden');
  }
}

/** 类别切换时更新表单布局 */
export function _onFormCategoryChange() {
  const cat = document.getElementById('todo-category')?.value;
  const title = document.querySelector('#custom-modal h3');
  const scheduleBlock = document.getElementById('todo-schedule-block');
  const dateBlock = document.getElementById('todo-date-block');

  if (title) {
    title.textContent = cat === 'schedule' ? '新建日程' : '新建待办';
  }

  if (cat === 'schedule') {
    scheduleBlock?.classList.remove('hidden');
    dateBlock?.classList.add('hidden');
  } else {
    scheduleBlock?.classList.add('hidden');
    dateBlock?.classList.remove('hidden');
  }
}

/** 打开新建待办/日程对话框 */
export async function addTodo() {
  const result = await renderTodoForm();
  if (result) {
    try {
      const res = await api.createTodo(result);
      if (res.ok) {
        toast('创建成功', 'success');
        await loadTodos();
      } else {
        const err = await res.text();
        console.error('[Todos] 创建失败:', err);
        toast(`创建失败: ${err}`, 'error');
      }
    } catch (e) {
      console.error('[Todos] 创建异常:', e);
      toast('网络错误', 'error');
    }
  }
}

/** 编辑待办/日程 */
export async function editTodo(id) {
  console.log('[Todos] editTodo id=', id, 'type=', typeof id);
  let todos = [];
  try {
    const res = await api.getTodos({});
    if (res.ok) todos = await res.json();
  } catch (e) {
    console.error('[Todos] 加载待办失败:', e);
    toast('加载待办失败，请检查网络', 'error');
    return;
  }

  const todo = todos.find(t => t.id === id);
  if (!todo) {
    console.error('[Todos] 未找到待办 id=', id, '共', todos.length, '条');
    toast('未找到该待办项，可能已被删除', 'warning');
    await loadTodos();
    return;
  }

  console.log('[Todos] 找到待办:', todo.title, 'status=', todo.status, 'category=', todo.category);

  const result = await renderTodoForm(todo);
  if (result) {
    // 日程不发送 status 字段
    if (result.category === 'schedule') {
      delete result.status;
    }
    try {
      const res = await api.updateTodo(id, result);
      if (res.ok) {
        toast('更新成功', 'success');
        await loadTodos();
      } else {
        const err = await res.text();
        console.error('[Todos] 更新失败:', err);
        toast(`更新失败: ${err}`, 'error');
      }
    } catch (e) {
      console.error('[Todos] 更新异常:', e);
      toast('网络错误', 'error');
    }
  }
}

/** 标记为完成 */
export async function completeTodo(id) {
  console.log('[Todos] completeTodo id=', id);
  try {
    const res = await api.updateTodo(id, { status: 'completed' });
    if (res.ok) { toast('已完成', 'success'); await loadTodos(); }
    else { const err = await res.text(); console.error('[Todos] 操作失败:', err); toast(`操作失败: ${err}`, 'error'); }
  } catch (e) { console.error('[Todos] 操作异常:', e); toast('网络错误', 'error'); }
}

/** 开始进行 */
export async function startTodo(id) {
  console.log('[Todos] startTodo id=', id);
  try {
    const res = await api.updateTodo(id, { status: 'in_progress' });
    if (res.ok) { await loadTodos(); }
    else { const err = await res.text(); console.error('[Todos] 操作失败:', err); toast(`操作失败: ${err}`, 'error'); }
  } catch (e) { console.error('[Todos] 操作异常:', e); toast('网络错误', 'error'); }
}

/** 删除 */
export async function deleteTodo(id) {
  if (!(await confirmDialog('确定要删除吗？此操作不可恢复。'))) return;
  console.log('[Todos] deleteTodo id=', id);
  try {
    const res = await api.deleteTodo(id);
    if (res.ok) { toast('已删除', 'success'); await loadTodos(); }
    else { const err = await res.text(); console.error('[Todos] 删除失败:', err); toast(`删除失败: ${err}`, 'error'); }
  } catch (e) { console.error('[Todos] 删除异常:', e); toast('网络错误', 'error'); }
}

/** 设置过滤并重新加载 */
export function setFilter(category, status) {
  if (category !== undefined) currentCategory = category;
  if (status !== undefined) currentStatus = status;

  document.querySelectorAll('#todos-category-tabs button').forEach(btn => {
    const cat = btn.dataset.category;
    btn.classList.toggle('bg-indigo-600', cat === currentCategory);
    btn.classList.toggle('text-white', cat === currentCategory);
    btn.classList.toggle('bg-gray-200', cat !== currentCategory);
    btn.classList.toggle('dark:bg-gray-600', cat !== currentCategory);
    btn.classList.toggle('text-gray-700', cat !== currentCategory);
    btn.classList.toggle('dark:text-gray-200', cat !== currentCategory);
  });

  document.querySelectorAll('#todos-status-tabs button').forEach(btn => {
    const st = btn.dataset.status;
    btn.classList.toggle('bg-indigo-100', st === currentStatus);
    btn.classList.toggle('dark:bg-indigo-900/40', st === currentStatus);
    btn.classList.toggle('text-indigo-700', st === currentStatus);
    btn.classList.toggle('dark:text-indigo-300', st === currentStatus);
    btn.classList.toggle('text-gray-600', st !== currentStatus);
    btn.classList.toggle('dark:text-gray-400', st !== currentStatus);
    btn.classList.toggle('hover:bg-gray-100', st !== currentStatus);
    btn.classList.toggle('dark:hover:bg-gray-700', st !== currentStatus);
  });

  loadTodos();
}

/** 获取当前过滤的 todos 数据（用于 AI 简报） */
export async function getTodosForBriefing() {
  try {
    const res = await api.getTodos({ status: 'pending' });
    if (res.ok) {
      const pending = await res.json();
      const res2 = await api.getTodos({ status: 'in_progress' });
      if (res2.ok) {
        const inProgress = await res2.json();
        return [...pending, ...inProgress];
      }
      return pending;
    }
  } catch (e) { /* ignore */ }
  return [];
}
