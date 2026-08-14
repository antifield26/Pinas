// ====== Antifield Cloud Service Worker v12 ======
// v12 变更：/api/ 响应不再缓存（敏感 JSON 曾永久滞留 CacheStorage 且离线显示陈旧数据）；
//          activate 时清空运行时缓存；manifest 加入预缓存清单（无版本则永不更新）
const CACHE_NAME = 'antifield-v12';
const RUNTIME_CACHE = 'antifield-runtime-v12';
const RUNTIME_MAX_AGE_MS = 24 * 60 * 60 * 1000; // 运行时缓存条目最长 24h

// 资源全部本地化（HTMX/Alpine/marked/DOMPurify 均在 assets/）
// 注：版本号与 HTML 引用（base.html ?v=）严格对齐，保证预缓存命中
const PRE_CACHE_URLS = [
  '/',
  '/assets/css/tailwind.min.css?v=17',
  '/assets/htmx.min.js?v=1',
  '/assets/alpine.min.js?v=1',
  '/assets/marked.min.js?v=1',
  '/assets/purify.min.js?v=1',
  '/assets/manifest.json?v=1',
];

// ====== Install ======
self.addEventListener('install', (event) => {
  console.log('[SW] v12 安装中...');
  event.waitUntil(
    caches.open(CACHE_NAME).then((cache) => {
      return cache.addAll(PRE_CACHE_URLS).catch((err) => {
        console.warn('[SW] 部分预缓存失败:', err);
      });
    }).then(() => self.skipWaiting())
  );
});

// ====== Activate: 清理旧缓存（含旧运行时缓存），通知客户端 ======
self.addEventListener('activate', (event) => {
  console.log('[SW] v12 已激活，清理旧缓存');
  event.waitUntil(
    caches.keys().then((keys) => {
      return Promise.all(
        keys.filter((k) => k !== CACHE_NAME)
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

  // 其他外部资源跳过（资源已全部本地化，无第三方 CDN 依赖）
  if (url.hostname !== location.hostname) return;

  // API 一律网络直连、绝不缓存：媒体体积大、JSON 含敏感数据，
  // 缓存会导致离线静默显示陈旧数据（离线提示永远不触发）
  if (url.pathname.startsWith('/api/')) return;

  // 静态资源 — Cache First
  if (url.pathname.startsWith('/assets/') || url.pathname.endsWith('.css')) {
    event.respondWith(cacheFirst(request));
    return;
  }

  // HTML — Network First（不写运行时缓存：页面随版本走，缓存即陈旧）
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
    return await fetch(request);
  } catch (e) {
    // HTML 离线兜底：仅当网络完全不可达时回退（静态资源走 cacheFirst 不受影响）
    const cached = await caches.match(request);
    if (cached) return cached;
    throw e;
  }
}

// 运行时缓存条目老化清理（防体积无界增长；activate 已整体清空，此为运行期兜底）
async function evictStaleRuntimeEntries() {
  try {
    const cache = await caches.open(RUNTIME_CACHE);
    const keys = await cache.keys();
    const now = Date.now();
    for (const key of keys) {
      const meta = await cache.match(key);
      if (!meta) continue;
      const dateHeader = meta.headers.get('date');
      const ts = dateHeader ? Date.parse(dateHeader) : 0;
      if (!ts || now - ts > RUNTIME_MAX_AGE_MS) {
        await cache.delete(key);
      }
    }
  } catch (e) {
    /* 清理失败无碍主流程 */
  }
}

self.addEventListener('message', (event) => {
  if (event.data === 'SKIP_WAITING') self.skipWaiting();
  if (event.data === 'CHECK_UPDATE') self.registration.update();
  if (event.data === 'EVICT_RUNTIME') evictStaleRuntimeEntries();
});
