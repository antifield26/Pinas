// ====== Antifield Cloud Service Worker ======
const CACHE_NAME = 'antifield-v2';
const RUNTIME_CACHE = 'antifield-runtime-v2';

// 需要预缓存的静态资源
const PRE_CACHE_URLS = [
  '/',
  '/assets/css/tailwind.min.css',
  '/assets/marked.min.js',
  '/assets/purify.min.js',
  '/assets/js/app.js',
  '/assets/js/api.js',
  '/assets/js/state.js',
  '/assets/js/utils.js',
  '/assets/js/constants.js',
  '/assets/js/components/toast.js',
  '/assets/js/components/modal.js',
  '/assets/js/components/file-table.js',
  '/assets/js/views/home.js',
  '/assets/js/views/drive.js',
  '/assets/js/views/trash.js',
  '/assets/js/views/admin.js',
  '/assets/js/views/share.js',
  '/assets/js/views/links.js',
  '/assets/js/views/todos.js',
  '/assets/js/views/agent.js',
];

// ====== Install: 预缓存核心资源 ======
self.addEventListener('install', (event) => {
  console.log('[SW] 正在安装 v2...');
  event.waitUntil(
    caches.open(CACHE_NAME).then((cache) => {
      console.log('[SW] 预缓存 ' + PRE_CACHE_URLS.length + ' 个资源');
      return cache.addAll(PRE_CACHE_URLS).catch((err) => {
        console.warn('[SW] 部分预缓存失败:', err);
      });
    }).then(() => self.skipWaiting())
  );
});

// ====== Activate: 清理旧缓存 ======
self.addEventListener('activate', (event) => {
  console.log('[SW] 已激活');
  event.waitUntil(
    caches.keys().then((keys) => {
      return Promise.all(
        keys.filter((k) => k !== CACHE_NAME && k !== RUNTIME_CACHE)
          .map((k) => {
            console.log('[SW] 清理旧缓存:', k);
            return caches.delete(k);
          })
      );
    }).then(() => self.clients.claim())
  );
});

// ====== Fetch: 缓存策略 ======
self.addEventListener('fetch', (event) => {
  const { request } = event;
  const url = new URL(request.url);

  // 跳过非 GET 请求
  if (request.method !== 'GET') return;

  // 跳过 chrome-extension 等非 http(s) 请求
  if (!url.protocol.startsWith('http')) return;

  // --- 策略1: 静态资源 — Cache First (immutable) ---
  if (
    url.pathname.startsWith('/assets/') ||
    url.pathname === '/manifest.json' ||
    url.pathname.endsWith('.css') ||
    url.pathname.endsWith('.js') ||
    url.pathname.endsWith('.png') ||
    url.pathname.endsWith('.svg') ||
    url.pathname.endsWith('.ico')
  ) {
    event.respondWith(cacheFirst(request));
    return;
  }

  // --- 策略2: API 请求 — Network First, 离线时返回缓存 ---
  if (url.pathname.startsWith('/api/')) {
    event.respondWith(networkFirst(request));
    return;
  }

  // --- 策略3: HTML/导航 — Network First ---
  if (request.mode === 'navigate') {
    event.respondWith(networkFirst(request));
    return;
  }

  // --- 策略4: 其他 — Network First ---
  event.respondWith(networkFirst(request));
});

// ====== 缓存策略函数 ======

// Cache First: 优先从缓存获取，缓存未命中时网络请求并缓存
async function cacheFirst(request) {
  const cached = await caches.match(request);
  if (cached) return cached;

  try {
    const response = await fetch(request);
    if (response.ok) {
      const cache = await caches.open(CACHE_NAME);
      cache.put(request, response.clone());
    }
    return response;
  } catch (e) {
    // 离线时返回自定义离线页面（仅 HTML 请求）
    if (request.mode === 'navigate') {
      const offlineCache = await caches.match('/');
      if (offlineCache) return offlineCache;
    }
    return new Response('离线状态，请检查网络连接', { status: 503 });
  }
}

// Network First: 优先从网络获取，失败时回退到缓存
async function networkFirst(request) {
  try {
    const response = await fetch(request);
    if (response.ok) {
      const cache = await caches.open(RUNTIME_CACHE);
      cache.put(request, response.clone());
    }
    return response;
  } catch (e) {
    const cached = await caches.match(request);
    if (cached) return cached;
    // 对 API 请求返回友好的离线 JSON
    if (request.url.includes('/api/')) {
      return new Response(
        JSON.stringify({ error: '离线状态，操作将在恢复网络后同步' }),
        { status: 503, headers: { 'Content-Type': 'application/json' } }
      );
    }
    throw e;
  }
}

// ====== 消息处理 ======
self.addEventListener('message', (event) => {
  if (event.data === 'SKIP_WAITING') {
    self.skipWaiting();
  }
  if (event.data === 'CHECK_UPDATE') {
    self.registration.update();
  }
});
