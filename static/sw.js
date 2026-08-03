// ====== Antifield Cloud Service Worker v6 ======
const CACHE_NAME = 'antifield-v6';
const RUNTIME_CACHE = 'antifield-runtime-v6';

// CDN 资源也纳入预缓存（支持离线使用）
const ASSETS_VERSION = 'v6';
const PRE_CACHE_URLS = [
  '/',
  `/assets/css/tailwind.min.css?v=${ASSETS_VERSION}`,
  '/assets/marked.min.js',
  '/assets/purify.min.js',
  // HTMX + Alpine CDN — 预缓存以支持离线
  'https://unpkg.com/htmx.org@2.0.4',
  'https://unpkg.com/alpinejs@3.14.9',
];

// ====== Install ======
self.addEventListener('install', (event) => {
  console.log('[SW] v6 安装中...');
  event.waitUntil(
    caches.open(CACHE_NAME).then((cache) => {
      return cache.addAll(PRE_CACHE_URLS).catch((err) => {
        console.warn('[SW] 部分预缓存失败（CDN 可能不可用）:', err);
      });
    }).then(() => self.skipWaiting())
  );
});

// ====== Activate: 清理旧缓存，通知客户端 ======
self.addEventListener('activate', (event) => {
  console.log('[SW] v6 已激活，清理旧缓存');
  event.waitUntil(
    caches.keys().then((keys) => {
      return Promise.all(
        keys.filter((k) => k !== CACHE_NAME && k !== RUNTIME_CACHE)
          .map((k) => caches.delete(k))
      );
    }).then(() => self.clients.claim())
  );
  // 不再自动 reload — 由客户端决定何时更新
});

// ====== Fetch ======
self.addEventListener('fetch', (event) => {
  const { request } = event;
  const url = new URL(request.url);

  if (request.method !== 'GET') return;
  if (!url.protocol.startsWith('http')) return;
  if (url.pathname === '/sw.js') return;

  // CDN 资源 — Cache First（预缓存中已有，离线可用）
  if (url.hostname === 'unpkg.com') {
    event.respondWith(cacheFirst(request));
    return;
  }

  // 其他外部资源跳过
  if (url.hostname !== location.hostname) return;

  // 静态资源 — Cache First
  if (url.pathname.startsWith('/assets/') || url.pathname.endsWith('.css')) {
    event.respondWith(cacheFirst(request));
    return;
  }

  // API + HTML — Network First
  event.respondWith(networkFirst(request));
});

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
    return new Response('离线状态，请检查网络连接', { status: 503 });
  }
}

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
    if (request.url.includes('/api/')) {
      return new Response(
        JSON.stringify({ error: '离线状态' }),
        { status: 503, headers: { 'Content-Type': 'application/json' } }
      );
    }
    throw e;
  }
}

self.addEventListener('message', (event) => {
  if (event.data === 'SKIP_WAITING') self.skipWaiting();
  if (event.data === 'CHECK_UPDATE') self.registration.update();
});
