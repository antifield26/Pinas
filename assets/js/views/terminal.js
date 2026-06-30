// ====== SSH Web 终端视图 ======
import { toast } from '../components/toast.js';
import { showModal } from '../components/modal.js';

let term = null;
let ws = null;
let fitAddon = null;

export function openTerminal() {
  const content = `
    <div class="space-y-3">
      <div class="grid grid-cols-2 gap-3">
        <div>
          <label class="block text-xs text-gray-500 mb-1">主机地址</label>
          <input type="text" id="ssh-host" value="raspberrypi.local" class="w-full px-3 py-2 border border-gray-200 dark:border-gray-600 rounded-lg text-sm bg-gray-50 dark:bg-gray-700 text-gray-800 dark:text-gray-200">
        </div>
        <div>
          <label class="block text-xs text-gray-500 mb-1">端口</label>
          <input type="number" id="ssh-port" value="22" class="w-full px-3 py-2 border border-gray-200 dark:border-gray-600 rounded-lg text-sm bg-gray-50 dark:bg-gray-700 text-gray-800 dark:text-gray-200">
        </div>
      </div>
      <div class="grid grid-cols-2 gap-3">
        <div>
          <label class="block text-xs text-gray-500 mb-1">用户名</label>
          <input type="text" id="ssh-username" value="pi" class="w-full px-3 py-2 border border-gray-200 dark:border-gray-600 rounded-lg text-sm bg-gray-50 dark:bg-gray-700 text-gray-800 dark:text-gray-200">
        </div>
        <div>
          <label class="block text-xs text-gray-500 mb-1">密码</label>
          <input type="password" id="ssh-password" class="w-full px-3 py-2 border border-gray-200 dark:border-gray-600 rounded-lg text-sm bg-gray-50 dark:bg-gray-700 text-gray-800 dark:text-gray-200">
        </div>
      </div>
    </div>
  `;

  showModal({
    title: 'SSH 终端',
    body: content,
    size: 'md',
    buttons: [
      { text: '取消', class: 'btn-secondary', value: false },
      { text: '连接', class: 'bg-green-600 hover:bg-green-700 text-white px-4 py-2 rounded-lg text-sm font-medium transition', value: 'connect' },
    ],
    getResult: () => {
      const host = document.getElementById('ssh-host')?.value.trim();
      const port = document.getElementById('ssh-port')?.value.trim();
      const username = document.getElementById('ssh-username')?.value.trim();
      const password = document.getElementById('ssh-password')?.value;
      if (!host || !username) {
        toast('请填写主机地址和用户名', 'warning');
        return undefined;
      }
      return { host, port: port || '22', username, password };
    },
  }).then((result) => {
    if (result && result.host) {
      connectAndOpen(result);
    }
  });
}

async function connectAndOpen(params) {
  // 如果已有连接，先断开
  if (ws) disconnect();

  const wsProtocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
  const token = localStorage.getItem('cloud_auth_token');
  const query = new URLSearchParams({
    host: params.host,
    port: params.port,
    username: params.username,
  });
  const wsUrl = `${wsProtocol}//${window.location.host}/api/ssh/ws?${query}&token=${encodeURIComponent(token)}`;

  // 显示终端模态框
  const modal = document.getElementById('ssh-modal');
  const title = document.getElementById('ssh-title');
  if (title) title.innerText = `SSH: ${params.username}@${params.host}`;
  if (modal) modal.classList.remove('hidden');

  const container = document.getElementById('ssh-terminal-container');
  if (!container) return;

  // 等待 xterm.js 加载
  await ensureXtermLoaded();

  // 初始化终端
  term = new Terminal({
    cursorBlink: true,
    fontSize: 14,
    fontFamily: 'Consolas, "Courier New", monospace',
    theme: {
      background: '#1e1e1e',
      foreground: '#d4d4d4',
      cursor: '#ffffff',
      selectionBackground: '#264f78',
    },
    cols: 80,
    rows: 24,
    allowProposedApi: true,
  });

  // xterm v4: FitAddon 是命名空间对象，构造函数在 .FitAddon 中
  const FA = typeof FitAddon === 'function' ? FitAddon : (FitAddon?.FitAddon || FitAddon);
  fitAddon = new FA();
  term.loadAddon(fitAddon);
  term.open(container);

  // 延迟 fit 让 DOM 渲染完成
  setTimeout(() => { try { fitAddon.fit(); } catch(e) {} }, 100);

  // 窗口大小变化时自适应
  const resizeObserver = new ResizeObserver(() => {
    try { fitAddon.fit(); } catch(e) {}
    // 通知服务端终端大小变化
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ cols: term.cols, rows: term.rows }));
    }
  });
  resizeObserver.observe(container);

  term.onResize(({ cols, rows }) => {
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ cols, rows }));
    }
  });

  // 连接 WebSocket
  term.write('正在连接...\r\n');
  ws = new WebSocket(wsUrl);
  ws.binaryType = 'arraybuffer';

  ws.onopen = () => {
    // 通过首条 WebSocket 消息发送密码（不经过 URL）
    ws.send(JSON.stringify({ password: params.password }));
    term.write('WebSocket 已连接，正在建立 SSH...\r\n');
  };

  ws.onmessage = (event) => {
    if (event.data instanceof ArrayBuffer) {
      const decoder = new TextDecoder();
      term.write(decoder.decode(event.data));
    } else if (typeof event.data === 'string') {
      // 尝试解析为 JSON resize 确认，否则写入终端
      try {
        const msg = JSON.parse(event.data);
        if (msg.resize_ok) return;
      } catch {}
      term.write(event.data);
    }
  };

  ws.onerror = () => {
    term.write('\r\n\x1b[31mWebSocket 连接错误\x1b[0m\r\n');
  };

  ws.onclose = () => {
    term.write('\r\n\x1b[33m连接已断开\x1b[0m\r\n');
    ws = null;
  };

  // 键盘输入 → WebSocket
  term.onData((data) => {
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(data);
    }
  });

  // Ctrl+Shift+D 断开
  term.attachCustomKeyEventHandler((e) => {
    if (e.ctrlKey && e.shiftKey && e.key === 'D') {
      disconnect();
      return false;
    }
    return true;
  });
}

export function disconnect() {
  if (ws) {
    ws.close();
    ws = null;
  }
  if (term) {
    term.dispose();
    term = null;
  }
  document.getElementById('ssh-modal')?.classList.add('hidden');
}

async function ensureXtermLoaded() {
  if (typeof Terminal !== 'undefined' && typeof FitAddon !== 'undefined') return;

  if (!document.querySelector('link[href*="xterm"]')) {
    const css = document.createElement('link');
    css.rel = 'stylesheet';
    css.href = 'https://cdn.jsdelivr.net/npm/xterm@4.19.0/css/xterm.css';
    document.head.appendChild(css);
  }

  if (typeof Terminal === 'undefined') {
    await new Promise((resolve, reject) => {
      const script = document.createElement('script');
      script.src = 'https://cdn.jsdelivr.net/npm/xterm@4.19.0/lib/xterm.min.js';
      script.onload = resolve;
      script.onerror = reject;
      document.head.appendChild(script);
    });
  }

  if (typeof FitAddon === 'undefined') {
    await new Promise((resolve, reject) => {
      const script = document.createElement('script');
      script.src = 'https://cdn.jsdelivr.net/npm/xterm-addon-fit@0.7.0/lib/xterm-addon-fit.min.js';
      script.onload = resolve;
      script.onerror = reject;
      document.head.appendChild(script);
    });
  }
}
