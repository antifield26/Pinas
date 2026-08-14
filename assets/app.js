// ====== Antifield App 主脚本（自 base.html 内联块外置，CSP script-src 不再依赖 unsafe-inline） ======
// 外部脚本天然只执行一次：hx-boost 导航替换 innerHTML 不会重建本文件
    // ====== Antifield App 命名空间 ======
    window.App = {
      showToast: function(msg, type) {
        type = type || 'info';
        window.dispatchEvent(new CustomEvent('show-toast', { detail: { message: msg, type: type } }));
      },
      closeModal: function() {
        var modal = document.getElementById('modal-container');
        modal.innerHTML = '';
        modal.classList.add('hidden');
        modal.classList.remove('flex');
      },
      // 云盘路径导航统一入口（路径唯一来源 = #drive-current-path，先更新再刷新列表与面包屑）
      navigateTo: function(path) {
        var el = document.getElementById('drive-current-path');
        if (el) el.value = path;
        htmx.ajax('GET', '/drive/list', { target: '#drive-file-list', values: { path: path } });
        htmx.ajax('GET', '/drive/breadcrumbs', { target: '#drive-breadcrumbs', values: { path: path } });
      },
      // 返回上级目录
      goParent: function() {
        var el = document.getElementById('drive-current-path');
        var v = el ? el.value || '/' : '/';
        if (v === '/' || v === '') return;
        App.navigateTo(v.substring(0, v.lastIndexOf('/')) || '/');
      },
      // 媒体加载失败：移除失败元素并显示静态 fallback（不拼接用户数据进 innerHTML，防 XSS）
      showMediaError: function(el) {
        var fallback = document.getElementById('media-fallback');
        if (el) el.remove();
        if (fallback) fallback.classList.remove('hidden');
      },
      // Markdown 渲染（AI 回复 / 文件预览共用）：marked.parse 后必须经 DOMPurify 消毒，杜绝 XSS。
      // marked/purify 懒加载：首次渲染时动态注入（70KB 不再全站加载）；就绪前先显示纯文本占位，
      // 两库均加载完成后统一回填渲染（动态脚本异步执行，不可依赖插入顺序）
      renderMarkdown: function(el, raw) {
        var apply = function() { el.innerHTML = DOMPurify.sanitize(marked.parse(raw)); };
        if (typeof marked !== 'undefined' && typeof DOMPurify !== 'undefined') {
          apply();
          return;
        }
        el.textContent = raw;
        if (!window.__mdPending) window.__mdPending = [];
        window.__mdPending.push(apply);
        if (window.__mdLoading) return;
        window.__mdLoading = true;
        var tryRun = function() {
          if (typeof marked !== 'undefined' && typeof DOMPurify !== 'undefined') {
            var q = window.__mdPending || [];
            window.__mdPending = null;
            window.__mdLoading = false;
            q.forEach(function(f) { f(); });
          }
        };
        ['/assets/marked.min.js?v=1', '/assets/purify.min.js?v=1'].forEach(function(src) {
          var s = document.createElement('script');
          s.src = src;
          s.onload = tryRun;
          s.onerror = function() { window.__mdLoading = false; };
          document.head.appendChild(s);
        });
      }
    };
    // 向后兼容别名（preview.html 的 onerror="showMediaError(this)" 在全局作用域解析，必须有此别名）
    window.showToast = App.showToast;
    window.closeModal = App.closeModal;
    window.showMediaError = App.showMediaError;

    // 云盘导航委托：目录行 / 打开按钮 (data-nav-path) 与面包屑 (data-breadcrumb-path) 统一入口。
    // 用户数据不进入内联 JS 字符串（防存储型 XSS），统一从 data-* 属性读取。
    document.body.addEventListener('click', function(e) {
      var el = e.target.closest('[data-nav-path], [data-breadcrumb-path]');
      if (!el) return;
      e.preventDefault();
      // 从搜索结果跳转目录：退出全局搜索态（清空勾选与搜索词），navigateTo 以正常目录浏览刷新
      var gs = document.getElementById('global-search');
      if (gs && gs.checked) gs.checked = false;
      var si = document.querySelector('input[name="search"]');
      if (si && si.value) si.value = '';
      var path = el.getAttribute('data-nav-path') || el.getAttribute('data-breadcrumb-path') || '';
      App.navigateTo(path);
      if (el.hasAttribute('data-breadcrumb-path')) {
        // 面包屑点击时同步刷新面包屑自身（hx-on 旧实现的功能迁移）
        htmx.ajax('GET', '/drive/breadcrumbs', { target: '#drive-breadcrumbs', values: { path: path } });
      }
    });

    // PWA Service Worker
    if ('serviceWorker' in navigator) {
      navigator.serviceWorker.register('/sw.js?v=14', { scope: '/' }).then(function(r) {
        console.log('[PWA] SW registered');
        r.addEventListener('updatefound', function() {
          var newWorker = r.installing;
          if (newWorker) {
            newWorker.addEventListener('statechange', function() {
              if (newWorker.state === 'installed' && navigator.serviceWorker.controller) {
                console.log('[PWA] 新版本已就绪，刷新页面后生效');
              }
            });
          }
        });
      }).catch(function() { console.warn('[PWA] SW 注册失败'); });
    }
    window.addEventListener('online', function() { document.getElementById('offline-banner')?.classList.add('hidden'); });
    window.addEventListener('offline', function() { document.getElementById('offline-banner')?.classList.remove('hidden'); });

    // HTMX: 401 → redirect to login
    document.body.addEventListener('htmx:beforeSwap', function(e) {
      if (e.detail.xhr && e.detail.xhr.status === 401) {
        window.location.href = '/login';
        e.detail.shouldSwap = false;
        return;
      }
      // 离线降级：SW 兜底返回 503（text/plain）时保留旧内容 + 提示（不把错误文案 swap 进容器）
      if (e.detail.xhr && e.detail.xhr.status === 503) {
        e.detail.shouldSwap = false;
        App.showToast('网络连接已断开，请检查网络后重试', 'error');
      }
    });

    // 服务端文件操作失败提示：HX-Trigger: {"toastError": "..."} → 错误 Toast
    // （历史实现错误被 fallback 列表静默吞掉，用户看到"操作成功"的假象）
    document.body.addEventListener('toastError', function(e) {
      App.showToast((e.detail && e.detail.value) ? e.detail.value : '操作失败', 'error');
    });

    // Alpine Toast component
    document.addEventListener('alpine:init', function() {
      Alpine.data('toasts', function() { return {
        items: [],
        add: function(msg) {
          var id = Date.now();
          this.items.push({ id: id, message: msg.message, type: msg.type });
          var self = this;
          setTimeout(function() { self.items = self.items.filter(function(t) { return t.id !== id; }); }, msg.duration || 4000);
        }
      }});
    });

    // 片段过渡动画映射：目标容器 → 新内容动画类
    // （htmx-added 标记本次 swap 新增的元素，避免 beforeend 追加时旧元素重复触发）
    var FRAGMENT_ANIMATIONS = {
      'drive-breadcrumbs': 'animate-fade-in',
      'quota-bar': 'animate-fade-in',
      'links-container': 'animate-fade-in',
      'trash-content': 'animate-fade-in',
      'admin-content': 'animate-fade-in',
      'conv-list': 'animate-fade-in',
      'agent-chat-messages': 'animate-slide-up',
      'home-chat-messages': 'animate-slide-up',
      'briefing-result': 'animate-slide-up'
    };

    // Modal: show when content loaded, hide when emptied
    // 注：模态框内容来自服务端 Askama 模板（自动转义），无第三方消毒依赖
    // 页面导航过渡（hx-boost 整页替换）：请求开始浅淡出 → 新页面淡入上移
    // 离场只淡到 0.6（不白屏）；请求失败/401 立即恢复
    document.body.addEventListener('htmx:beforeRequest', function(e) {
      if (e.detail.boosted) document.body.classList.add('animate-page-leave');
    });
    document.body.addEventListener('htmx:afterRequest', function(e) {
      if (e.detail.boosted && !e.detail.successful) document.body.classList.remove('animate-page-leave');
    });
    // 进场动画结束清理类，避免残留
    document.body.addEventListener('animationend', function(e) {
      if (e.animationName === 'page-in') document.body.classList.remove('animate-page-enter');
    });
    // 浏览器前进/后退（htmx history restore）同样淡入
    document.body.addEventListener('htmx:restored', function() {
      document.body.classList.add('animate-page-enter');
    });

    document.body.addEventListener('htmx:afterSwap', function(e) {
      var target = e.detail.target;
      if (!target) return;

      // 过期响应防护：快速连点目录时，旧请求的列表可能后到。
      // 若本次列表请求的 path 已不是当前路径，用最新路径重发（幂等刷新）。
      if (target.id === 'drive-file-list') {
        var el = document.getElementById('drive-current-path');
        var reqPath = e.detail.requestConfig && e.detail.requestConfig.parameters && e.detail.requestConfig.parameters.path;
        if (el && reqPath && reqPath !== el.value) {
          App.navigateTo(el.value);
          return;
        }
      }

      // 整页导航（hx-boost）：离场浅淡结束，切换为进场淡入
      if (e.detail.boosted) {
        document.body.classList.remove('animate-page-leave');
        document.body.classList.add('animate-page-enter');
        return;
      }

      // 模态框：注入内容缩放淡入（离场为同步清空，不做动画）
      var modal = document.getElementById('modal-container');
      if (target === modal) {
        var hasContent = !!modal.innerHTML.trim();
        modal.classList.toggle('hidden', !hasContent);
        modal.classList.toggle('flex', hasContent);
        if (hasContent) {
          var content = modal.firstElementChild;
          if (content) content.classList.add('animate-modal-in');
        }
        return;
      }

      // 片段过渡：给本次 swap 新增的内容挂动画类（drive-file-list/todos 由模板内交错入场处理）
      var anim = FRAGMENT_ANIMATIONS[target.id];
      if (anim) {
        Array.from(target.children).forEach(function(child) {
          if (child.classList.contains('htmx-added')) child.classList.add(anim);
        });
      }
    });
    // 模态框内表单提交成功后自动关闭
    document.body.addEventListener('htmx:afterRequest', function(e) {
      if (!e.detail.successful) return;
      var el = e.detail.elt;
      if (el.closest && el.closest('#modal-container')) App.closeModal();
    });
    document.addEventListener('keydown', function(e) { if (e.key === 'Escape') App.closeModal(); });

    // 全局文件上传函数 — 10 MB 分片 + 并发 + 断点续传 + 自动重试
    // ====== 单个文件上传核心（分片/并发3/重试3/秒传） ======
    // onProgress(pct, msg) 回调驱动进度 UI（队列面板）；signal 支持取消
    window.uploadOneFile = async function(file, path, onProgress, signal) {
      onProgress = onProgress || function() {};
      var CHUNK_MB = 10;
      var CONCURRENCY = 3;
      var MAX_RETRIES = 3;
      var chunkBytes = CHUNK_MB * 1024 * 1024;
      var totalChunks = Math.max(1, Math.ceil(file.size / chunkBytes));
      // identifier 由内容哈希派生(前 1MB + 文件大小 SHA-256)：
      // 同内容重传 → 同 identifier → /api/files/check 命中 → 秒传/跨会话断点续传真实生效。
      // 非安全上下文(纯 HTTP)无 crypto.subtle 时回退随机串。
      var identifier = 'f' + Date.now().toString(36) + Math.random().toString(36).slice(2, 8);
      try {
        if (window.crypto && crypto.subtle && file.size > 0) {
          var head = file.slice(0, 1024 * 1024);
          var headBuf = await head.arrayBuffer();
          var sizeBuf = new ArrayBuffer(8);
          new DataView(sizeBuf).setUint32(0, file.size >>> 0, true);
          new DataView(sizeBuf).setUint32(4, Math.floor(file.size / 0x100000000), true);
          var combined = new Uint8Array(headBuf.byteLength + 8);
          combined.set(new Uint8Array(headBuf), 0);
          combined.set(new Uint8Array(sizeBuf), headBuf.byteLength);
          var digest = await crypto.subtle.digest('SHA-256', combined);
          var hex = '';
          new Uint8Array(digest).forEach(function(b) { hex += b.toString(16).padStart(2, '0'); });
          identifier = 'f' + hex.slice(0, 32);
        }
      } catch (_) { /* crypto 不可用，保持随机回退 */ }
      // normalize path: collapse slashes, ensure leading /
      var raw = (path || '').replace(/\/+/g, '/');
      var parent = (raw === '/' || raw === '') ? '' : raw.replace(/\/$/, '');
      var uploadedCount = 0; // pre-existing + completed-this-session

      function setProgress(pctVal, msg) {
        onProgress(pctVal, msg);
      }

      // send a single chunk with exponential-backoff retry (1s / 2s / 4s)
      function sendChunk(index) {
        var start = index * chunkBytes;
        var end = Math.min(start + chunkBytes, file.size);
        var blob = file.slice(start, end);
        var url = '/api/files/upload_chunk?identifier=' + identifier
          + '&chunk_index=' + index + '&total_chunks=' + totalChunks;

        return new Promise(function(resolve, reject) {
          function attempt(retry) {
            if (signal && signal.aborted) { reject(new Error('已取消')); return; }
            var xhr = new XMLHttpRequest();
            xhr.open('POST', url);
            xhr.withCredentials = true;
            xhr.timeout = 120000;
            var onAbort = function() { xhr.abort(); reject(new Error('已取消')); };
            if (signal) signal.addEventListener('abort', onAbort);
            var cleanup = function() { if (signal) signal.removeEventListener('abort', onAbort); };
            xhr.onload = function() {
              cleanup();
              if (xhr.status === 200) { resolve(); return; }
              if (retry < MAX_RETRIES && xhr.status >= 500) {
                setTimeout(function() { attempt(retry + 1); }, Math.pow(2, retry) * 1000);
              } else {
                reject(new Error('HTTP ' + xhr.status));
              }
            };
            xhr.onerror = function() {
              cleanup();
              if (retry < MAX_RETRIES) {
                setTimeout(function() { attempt(retry + 1); }, Math.pow(2, retry) * 1000);
              } else { reject(new Error('Network error')); }
            };
            xhr.ontimeout = function() {
              cleanup();
              if (retry < MAX_RETRIES) {
                setTimeout(function() { attempt(retry + 1); }, Math.pow(2, retry) * 1000);
              } else { reject(new Error('Timeout')); }
            };
            var fd = new FormData();
            fd.append('file', blob, file.name);
            xhr.send(fd);
          }
          attempt(0);
        });
      }

      // --- Step 1: check for existing chunks (resume / instant upload) ---
      setProgress(0, file.name + ' 检查断点...');
      var pending = [];
      try {
        // file_name/parent_path 参与秒传判定：内容曾存在于其他路径时不误报秒传（L5）
        var cr = await fetch('/api/files/check?identifier=' + identifier
          + '&file_name=' + encodeURIComponent(file.name)
          + '&parent_path=' + encodeURIComponent(parent), { credentials: 'same-origin', signal: signal });
        if (cr.ok) {
          var cd = await cr.json();
          if (cd.exists) { setProgress(100, '秒传成功'); return; }
          var have = new Set(cd.uploaded_chunks || []);
          uploadedCount = have.size;
          for (var i = 0; i < totalChunks; i++) { if (!have.has(i)) pending.push(i); }
        }
      } catch (_) { /* check failed — upload all chunks */ }
      if (!pending.length) {
        for (var i = 0; i < totalChunks; i++) pending.push(i);
        uploadedCount = 0;
      }

      // --- Step 2: upload pending chunks with concurrency limiter ---
      if (pending.length > 0) {
        var totalPending = pending.length;
        var completedSession = 0;
        var cursor = 0;
        var active = 0;
        var firstError = null;

        await new Promise(function(resolveAll, rejectAll) {
          function launch() {
            while (active < CONCURRENCY && cursor < pending.length && !firstError) {
              var idx = pending[cursor++];
              active++;
              setProgress(Math.round((uploadedCount + completedSession) / totalChunks * 90),
                file.name + ' ' + (uploadedCount + completedSession) + '/' + totalChunks);
              sendChunk(idx).then(function() {
                completedSession++;
                active--;
                setProgress(Math.round((uploadedCount + completedSession) / totalChunks * 90),
                  file.name + ' ' + (uploadedCount + completedSession) + '/' + totalChunks);
                if (completedSession >= totalPending) resolveAll(); else launch();
              }).catch(function(e) {
                firstError = e;
                rejectAll(e);
              });
            }
          }
          launch();
        });
      }

      // --- Step 3: merge chunks on server ---
      setProgress(95, '合并中...');
      var mr = await fetch('/api/files/merge', {
        method: 'POST', credentials: 'same-origin',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ identifier: identifier, file_name: file.name, parent_path: parent })
      });
      if (mr.ok) {
        setProgress(100, '完成');
      } else {
        setProgress(0, '合并失败');
        throw new Error('Merge failed');
      }
    };

    // 旧接口兼容层：单文件上传 + 旧模态进度条（新代码走 UploadQueue）
    window.uploadFile = async function(file, path) {
      var progress = document.getElementById('upload-progress');
      var bar = document.getElementById('upload-bar');
      var pct = document.getElementById('upload-pct');
      var status = document.getElementById('upload-status');
      if (progress) progress.classList.remove('hidden');
      await window.uploadOneFile(file, path, function(p, m) {
        if (bar) bar.style.width = p + '%';
        if (pct) pct.textContent = p + '%';
        if (status && m !== undefined) status.textContent = m;
      });
    };

    // ====== 上传队列管理器（并发 3；支持文件夹上传与取消） ======
    window.UploadQueue = {
      items: [],
      running: 0,
      maxConcurrent: 3,
      controllers: [],

      // list: [{file, rel}]；rel 为相对路径(如 "子目录/a.txt"，纯文件时=文件名)
      enqueue: function(list, targetPath) {
        var self = this;
        list.forEach(function(it) {
          self.items.push({
            file: it.file, rel: it.rel || it.file.name,
            targetPath: targetPath, status: 'pending', pct: 0, msg: '', error: null
          });
        });
        this.render();
      },

      start: function() { this.pump(); },

      pump: function() {
        var self = this;
        while (this.running < this.maxConcurrent) {
          var next = null;
          for (var i = 0; i < this.items.length; i++) {
            if (this.items[i].status === 'pending') { next = this.items[i]; break; }
          }
          if (!next) { if (this.running === 0) this.finishCheck(); return; }
          this.runOne(next);
        }
      },

      runOne: function(item) {
        var self = this;
        item.status = 'uploading';
        this.running++;
        var controller = new AbortController();
        this.controllers.push(controller);
        // 目标目录 = 面板目标 + rel 的目录部分
        var path = item.targetPath;
        if (item.rel.indexOf('/') >= 0) {
          path = (path === '/' || path === '') ? '' : path.replace(/\/+$/, '');
          path += '/' + item.rel.substring(0, item.rel.lastIndexOf('/'));
        }
        window.uploadOneFile(item.file, path, function(pct, msg) {
          item.pct = pct; item.msg = msg || '';
          self.render();
        }, controller.signal).then(function() {
          item.status = 'done'; item.pct = 100;
        }).catch(function(e) {
          if (e && e.message === '已取消') { item.status = 'canceled'; }
          else { item.status = 'error'; item.error = e && e.message ? e.message : String(e); }
        }).then(function() {
          self.running--;
          self.controllers = self.controllers.filter(function(c) { return c !== controller; });
          self.render();
          self.pump();
        });
      },

      finishCheck: function() {
        var total = this.items.length;
        var finished = this.items.filter(function(i) {
          return i.status === 'done' || i.status === 'error' || i.status === 'canceled';
        }).length;
        if (total > 0 && finished === total) {
          var pathEl = document.getElementById('drive-current-path');
          var cur = pathEl ? pathEl.value : '/';
          htmx.ajax('GET', '/drive/list', { target: '#drive-file-list', values: { path: cur } });
          htmx.trigger('body', 'quotaRefresh');
          var panel = document.getElementById('upload-panel');
          if (panel) {
            var self = this;
            setTimeout(function() {
              panel.classList.add('hidden');
              self.items = [];
              self.render();
            }, 4000);
          }
        }
      },

      cancel: function() {
        this.controllers.forEach(function(c) { c.abort(); });
        this.controllers = [];
      },

      render: function() {
        var panel = document.getElementById('upload-panel');
        var listEl = document.getElementById('upload-queue-list');
        if (!panel || !listEl) return;
        var total = this.items.length;
        var doneCount = 0;
        for (var i = 0; i < this.items.length; i++) if (this.items[i].status === 'done') doneCount++;
        var pct = total ? Math.round(doneCount / total * 100) : 0;
        panel.classList.remove('hidden');
        document.getElementById('upload-panel-count').textContent = doneCount + '/' + total;
        document.getElementById('upload-panel-pct').textContent = pct + '%';
        document.getElementById('upload-panel-bar').style.width = pct + '%';
        listEl.innerHTML = '';
        var labels = { pending: '等待中', done: '✓ 完成', error: '失败', canceled: '已取消' };
        for (var j = 0; j < this.items.length; j++) {
          var it = this.items[j];
          var row = document.createElement('div');
          row.className = 'flex items-center gap-2 text-xs';
          var nameEl = document.createElement('span');
          nameEl.className = 'flex-1 truncate text-gray-700 dark:text-gray-300';
          nameEl.textContent = it.rel;
          var stEl = document.createElement('span');
          stEl.className = it.status === 'done' ? 'text-emerald-500 shrink-0'
            : (it.status === 'error' ? 'text-red-500 shrink-0' : 'text-gray-400 shrink-0');
          stEl.textContent = labels[it.status] || ('上传中 ' + (it.pct || 0) + '%');
          row.appendChild(nameEl);
          row.appendChild(stEl);
          listEl.appendChild(row);
        }
      }
    };

    // ====== CSP 收敛：模板内联脚本与内联事件处理器外置（data-* 属性 + document 事件委托） ======

    // --- 设置表单（settings_form.html）---
    App.saveSettings = async function(e) {
      e.preventDefault();
      const body = {
        deepseek_api_key: document.getElementById('settings-apikey').value.trim() || null,
        deepseek_api_base: document.getElementById('settings-apibase').value.trim() || null,
        deepseek_model: document.getElementById('settings-model').value.trim() || null,
        temperature: parseFloat(document.getElementById('settings-temperature').value),
        max_tokens: parseInt(document.getElementById('settings-max-tokens').value)
      };
      try {
        const res = await fetch('/api/agent/settings', {
          method: 'PUT', headers: { 'Content-Type': 'application/json' },
          credentials: 'same-origin', body: JSON.stringify(body)
        });
        if (res.ok) { App.closeModal(); App.showToast('设置已保存', 'success'); }
        else { App.showToast('保存失败', 'error'); }
      } catch(e) { App.showToast('网络错误', 'error'); }
      return false;
    };

    // --- 上传表单（upload_form.html）---
    App.handleUploadForm = function(e) {
      e.preventDefault();
      const files = document.getElementById('upload-file-input').files;
      if (!files.length) return false;
      const path = (document.getElementById('upload-target-path')?.value || '/').replace(/\/+/g, '/');
      // 入队上传（并发 3 + 进度面板由 UploadQueue 统一驱动），完成后面板自动刷新列表
      const list = [];
      for (let i = 0; i < files.length; i++) list.push({ file: files[i], rel: files[i].name });
      window.UploadQueue.enqueue(list, path);
      window.UploadQueue.start();
      App.closeModal();
      return false;
    };

    // --- 弹出式改密（password_change_form.html）---
    window.handlePwdModal = function(e) {
      e.preventDefault();
      var current = document.getElementById('pwd-current').value;
      var newPwd = document.getElementById('pwd-new').value;
      var confirm = document.getElementById('pwd-confirm').value;
      var errEl = document.getElementById('pwd-modal-error');
      var btn = document.getElementById('pwd-modal-submit');

      if (newPwd !== confirm) {
        errEl.textContent = '两次输入的新密码不一致';
        errEl.classList.remove('hidden');
        return false;
      }
      if (newPwd.length < 6 || newPwd.length > 128) {
        errEl.textContent = '新密码长度必须为 6-128 个字符';
        errEl.classList.remove('hidden');
        return false;
      }

      btn.textContent = '处理中...';
      btn.disabled = true;
      errEl.classList.add('hidden');

      fetch('/api/user/password', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        credentials: 'same-origin',
        body: JSON.stringify({ current_password: current, new_password: newPwd })
      }).then(function(res) {
        if (res.ok) {
          App.closeModal();
          App.showToast('密码修改成功', 'success');
        } else {
          return res.text().then(function(text) {
            errEl.textContent = text || '密码修改失败';
            errEl.classList.remove('hidden');
          });
        }
      }).catch(function() {
        errEl.textContent = '网络错误，请检查连接';
        errEl.classList.remove('hidden');
      }).finally(function() {
        btn.textContent = '更新密码';
        btn.disabled = false;
      });
      return false;
    };

    // --- 文件批量选择（file_table.html）---
    function toggleSelectAll(el) {
      document.querySelectorAll('.file-checkbox').forEach(cb => { cb.checked = el.checked; });
      updateBatchToolbar();
    }
    function updateBatchToolbar() {
      const checked = document.querySelectorAll('.file-checkbox:checked');
      const toolbar = document.getElementById('batch-toolbar');
      const count = document.getElementById('batch-count');
      if (checked.length > 0) {
        toolbar.classList.remove('hidden');
        count.textContent = '已选 ' + checked.length + ' 项';
        document.getElementById('select-all').checked = (checked.length === document.querySelectorAll('.file-checkbox').length);
      } else {
        toolbar.classList.add('hidden');
        document.getElementById('select-all').checked = false;
      }
    }
    async function batchDelete() {
      var names = Array.from(document.querySelectorAll('.file-checkbox:checked')).map(cb => cb.value);
      if (!names.length || !confirm('确定删除选中的 ' + names.length + ' 个文件？')) return;
      var path = document.getElementById('drive-current-path')?.value || '/';
      var res = await fetch('/api/files/delete_batch', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        credentials: 'same-origin',
        body: JSON.stringify({ names: names, current_path: path })
      });
      if (res.ok) {
        htmx.ajax('GET', '/drive/list', { target: '#drive-file-list', values: { path: path } });
      } else {
        alert('批量删除失败');
      }
    }
    function batchDownload() {
      var names = Array.from(document.querySelectorAll('.file-checkbox:checked')).map(cb => cb.value);
      if (!names.length) { alert('请先选择文件'); return; }
      var path = document.getElementById('drive-current-path')?.value || '/';
      var form = document.createElement('form');
      form.method = 'POST';
      form.action = '/api/files/download_zip';
      names.forEach(function(n) {
        var inp = document.createElement('input');
        inp.type = 'hidden'; inp.name = 'names'; inp.value = n;
        form.appendChild(inp);
      });
      var inp2 = document.createElement('input');
      inp2.type = 'hidden'; inp2.name = 'current_path'; inp2.value = path;
      form.appendChild(inp2);
      document.body.appendChild(form);
      form.submit();
      form.remove();
    }

    // --- 独立页主题切换（theme_head.html）---
    function toggleDark() {
      var html = document.documentElement;
      html.classList.toggle('dark');
      localStorage.setItem('theme', html.classList.contains('dark') ? 'dark' : 'light');
    }

    // --- 登录页（login.html）---
    var isRegister = false;
    function toggleRegister() {
      isRegister = !isRegister;
      document.getElementById('login-btn').textContent = isRegister ? '注册' : '验证并进入';
      document.getElementById('toggle-btn').textContent = isRegister ? '已有账号？点击登录' : '没有账号？点击注册';
      document.getElementById('login-error').classList.add('hidden');
    }
    async function handleLogin(e) {
      e.preventDefault();
      var username = document.getElementById('username').value.trim();
      var password = document.getElementById('password').value;
      var errEl = document.getElementById('login-error');
      var btn = document.getElementById('login-btn');

      if (!username || !password) {
        errEl.textContent = '请填写用户名和密码';
        errEl.classList.remove('hidden');
        return false;
      }

      btn.textContent = '处理中...';
      btn.disabled = true;
      errEl.classList.add('hidden');

      try {
        var endpoint = isRegister ? '/api/register' : '/api/login';
        var res = await fetch(endpoint, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ username: username, password: password })
        });
        if (res.ok) {
          var data = await res.json();
          // token 由服务器通过 httpOnly Cookie 自动设置，客户端不再存储
          if (data.must_change_pwd) {
            window.location.href = '/change-password';
          } else {
            // redirect 校验（M12）：站内绝对路径照旧；绝对 URL 仅允许同注册域
            var params = new URLSearchParams(window.location.search);
            var target = params.get('redirect') || '/';
            if (target.charAt(0) === '/') {
              if (target.charAt(1) === '/') target = '/';
            } else {
              try {
                var u = new URL(target);
                var host = window.location.hostname;
                var regDomain = host.split('.').slice(-2).join('.');
                var sameRegDomain = u.hostname === host ||
                  u.hostname === regDomain ||
                  u.hostname.endsWith('.' + regDomain);
                if (u.protocol !== 'https:' || !sameRegDomain) target = '/';
              } catch (err) {
                target = '/';
              }
            }
            window.location.href = target;
          }
        } else {
          var text = await res.text();
          errEl.textContent = text || '认证失败';
          errEl.classList.remove('hidden');
        }
      } catch (err) {
        errEl.textContent = '网络错误，请检查连接';
        errEl.classList.remove('hidden');
      }

      btn.textContent = isRegister ? '注册' : '验证并进入';
      btn.disabled = false;
      return false;
    }

    // --- 改密页（change_password.html）---
    async function handleChangePwd(e) {
      e.preventDefault();
      var current = document.getElementById('current-password').value;
      var newPwd = document.getElementById('new-password').value;
      var confirm = document.getElementById('confirm-password').value;
      var errEl = document.getElementById('error-msg');
      var btn = document.getElementById('submit-btn');

      if (newPwd !== confirm) {
        errEl.textContent = '两次输入的新密码不一致';
        errEl.classList.remove('hidden');
        return false;
      }

      if (newPwd.length < 6 || newPwd.length > 128) {
        errEl.textContent = '新密码长度必须为 6-128 个字符';
        errEl.classList.remove('hidden');
        return false;
      }

      btn.textContent = '处理中...';
      btn.disabled = true;
      errEl.classList.add('hidden');

      try {
        var res = await fetch('/api/user/password', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          credentials: 'same-origin',
          body: JSON.stringify({ current_password: current, new_password: newPwd })
        });
        if (res.ok) {
          window.location.href = '/';
        } else {
          var text = await res.text();
          errEl.textContent = text || '密码修改失败';
          errEl.classList.remove('hidden');
        }
      } catch (err) {
        errEl.textContent = '网络错误，请检查连接';
        errEl.classList.remove('hidden');
      }

      btn.textContent = '更新密码';
      btn.disabled = false;
      return false;
    }

    // --- 云盘上传（drive.html）：拖拽递归遍历 / 落盘 / 文件夹选择器 ---
    App.collectDropEntries = function(dataTransfer, cb) {
      var items = dataTransfer.items;
      var entries = [];
      for (var i = 0; i < items.length; i++) {
        var it = items[i];
        if (it && it.webkitGetAsEntry) entries.push(it.webkitGetAsEntry());
      }
      var anyDir = entries.some(function(e) { return e && e.isDirectory; });
      if (!entries.length || !anyDir) {
        // 降级：仅文件（无相对路径）
        var out = [];
        for (var j = 0; j < dataTransfer.files.length; j++) {
          out.push({ file: dataTransfer.files[j], rel: dataTransfer.files[j].name });
        }
        cb(out);
        return;
      }
      var collected = [];
      var left = entries.length;
      function walk(entry, base, done) {
        if (entry.isFile) {
          entry.file(function(f) { collected.push({ file: f, rel: base + f.name }); done(); }, done);
        } else if (entry.isDirectory) {
          var reader = entry.createReader();
          var all = [];
          function readAll() {
            reader.readEntries(function(batch) {
              if (batch.length) { all = all.concat(batch); readAll(); }
              else if (!all.length) { done(); }
              else {
                var childLeft = all.length;
                all.forEach(function(child) {
                  walk(child, base + entry.name + '/', function() { childLeft--; if (!childLeft) done(); });
                });
              }
            }, done);
          }
          readAll();
        } else { done(); }
      }
      entries.forEach(function(e) {
        if (e) walk(e, '', function() { left--; if (!left) cb(collected); });
        else { left--; if (!left) cb(collected); }
      });
    };
    App.handleDrop = function(e) {
      var path = (document.getElementById('drive-current-path')?.value || '/').replace(/\/+/g, '/');
      App.collectDropEntries(e.dataTransfer, function(list) {
        if (!list.length) return;
        window.UploadQueue.enqueue(list, path);
        window.UploadQueue.start();
      });
    };
    App.handleFolderInput = function(input) {
      var path = (document.getElementById('drive-current-path')?.value || '/').replace(/\/+/g, '/');
      var list = [];
      for (var i = 0; i < input.files.length; i++) {
        var f = input.files[i];
        list.push({ file: f, rel: f.webkitRelativePath || f.name });
      }
      input.value = '';
      if (!list.length) return;
      window.UploadQueue.enqueue(list, path);
      window.UploadQueue.start();
    };

    // --- 片段载入后初始化（new_folder_form 目标目录 / preview markdown / 视频续播）---
    App.initVideoResume = function(v) {
      if (v.dataset.resumeBound) return;
      v.dataset.resumeBound = '1';
      var key = 'pinas:pos:' + v.getAttribute('data-key');
      var saved = parseFloat(localStorage.getItem(key) || '0');
      if (saved > 0) {
        v.addEventListener('loadedmetadata', function() {
          if (saved > 5 && v.duration - saved > 10) v.currentTime = saved;
        }, { once: true });
      }
      var lastSave = 0;
      v.addEventListener('timeupdate', function() {
        var now = Date.now();
        if (now - lastSave > 5000) { lastSave = now; localStorage.setItem(key, String(v.currentTime)); }
      });
      v.addEventListener('ended', function() { localStorage.removeItem(key); });
    };
    App.initFragment = function(container) {
      // new_folder_form：目标目录默认值 = 当前浏览路径
      var np = document.getElementById('new-folder-path');
      if (np) np.value = document.getElementById('drive-current-path')?.value || '/';
      // preview：markdown 原文渲染
      var data = document.getElementById('markdown-data');
      var body = document.getElementById('markdown-body');
      if (data && body) {
        try { App.renderMarkdown(body, JSON.parse(data.textContent)); }
        catch (_) { body.textContent = '（渲染失败）'; }
      }
      // preview：视频续播
      var v = document.getElementById('video-player');
      if (v) App.initVideoResume(v);
    };

    // ====== AI 对话（agent.html）：AppAgent 命名空间 + SSE 流式 ======
    window.AppAgent = {
      currentConvId: null,
      streaming: false,

      // 渲染一条消息（纯 DOM + textContent，防注入；assistant 消息渲染 markdown）
      appendMessage: function(role, content, useMarkdown) {
        var box = document.getElementById('agent-chat-messages');
        if (!box) return;
        var wrapper = document.createElement('div');
        wrapper.className = role === 'user'
          ? 'flex gap-3 justify-end animate-slide-up'
          : 'flex gap-3 animate-slide-up';
        if (role === 'user') {
          var bubble = document.createElement('div');
          bubble.className = 'bg-indigo-600 text-white rounded-2xl rounded-br-md px-4 py-2.5 max-w-[80%] text-sm';
          var p = document.createElement('p');
          p.className = 'whitespace-pre-wrap break-words';
          p.textContent = content;
          bubble.appendChild(p);
          wrapper.appendChild(bubble);
        } else {
          var label = document.createElement('div');
          label.className = 'text-sm shrink-0 text-indigo-500 font-bold';
          label.textContent = 'AI';
          var bubble = document.createElement('div');
          bubble.className = 'bg-gray-100 dark:bg-gray-800 rounded-2xl rounded-bl-md px-4 py-2.5 max-w-[80%] text-sm text-gray-800 dark:text-gray-200';
          var p = document.createElement('p');
          p.className = 'whitespace-pre-wrap break-words';
          if (useMarkdown) {
            window.App.renderMarkdown(p, content);
          } else {
            p.textContent = content;
          }
          bubble.appendChild(p);
          wrapper.appendChild(label);
          wrapper.appendChild(bubble);
        }
        box.appendChild(wrapper);
        box.scrollTop = box.scrollHeight;
      },

      // 创建流式 AI 气泡，返回 { append(delta), finish() }
      startStreamBubble: function() {
        var box = document.getElementById('agent-chat-messages');
        var wrapper = document.createElement('div');
        wrapper.className = 'flex gap-3 animate-slide-up';
        var label = document.createElement('div');
        label.className = 'text-sm shrink-0 text-indigo-500 font-bold';
        label.textContent = 'AI';
        var bubble = document.createElement('div');
        bubble.className = 'bg-gray-100 dark:bg-gray-800 rounded-2xl rounded-bl-md px-4 py-2.5 max-w-[80%] text-sm text-gray-800 dark:text-gray-200';
        var p = document.createElement('p');
        p.className = 'whitespace-pre-wrap break-words';
        var thinking = document.createElement('span');
        thinking.className = 'text-gray-400';
        thinking.textContent = '思考中...';
        p.appendChild(thinking);
        bubble.appendChild(p);
        wrapper.appendChild(label);
        wrapper.appendChild(bubble);
        box.appendChild(wrapper);
        box.scrollTop = box.scrollHeight;
        var full = '';
        return {
          append: function(delta) {
            if (!full) thinking.remove();
            full += delta;
            p.textContent = full;
            box.scrollTop = box.scrollHeight;
          },
          finish: function() {
            thinking.remove();
            if (full.trim()) {
              window.App.renderMarkdown(p, full);
            }
            box.scrollTop = box.scrollHeight;
          }
        };
      },

      // 新建对话：创建后清空消息区并记 id（同时刷新桌面/移动端双列表）
      newConversation: function() {
        fetch('/api/conversations', { method: 'POST', credentials: 'same-origin' })
          .then(function(r) { return r.json(); })
          .then(function(conv) {
            AppAgent.currentConvId = conv.id;
            document.getElementById('agent-conversation-id').value = conv.id;
            var box = document.getElementById('agent-chat-messages');
            if (box) box.innerHTML = '';
            AppAgent.refreshConvList();
            AppAgent.toggleDrawer(false);
          })
          .catch(function() { App.showToast('创建对话失败', 'error'); });
      },

      // 加载指定对话的消息历史（assistant 消息渲染 markdown）
      loadConversation: function(id) {
        fetch('/api/conversations/' + id + '/messages', { credentials: 'same-origin' })
          .then(function(r) { return r.json(); })
          .then(function(messages) {
            AppAgent.currentConvId = id;
            document.getElementById('agent-conversation-id').value = id;
            var box = document.getElementById('agent-chat-messages');
            if (box) box.innerHTML = '';
            messages.forEach(function(m) {
              AppAgent.appendMessage(m.role, m.content, m.role === 'assistant');
            });
            AppAgent.toggleDrawer(false);
          })
          .catch(function() { App.showToast('加载对话失败', 'error'); });
      },

      // 刷新会话列表（桌面 #conv-list + 移动端 #conv-list-m）
      refreshConvList: function() {
        var active = AppAgent.currentConvId || '';
        htmx.ajax('GET', '/agent/conversations', { target: '#conv-list', swap: 'innerHTML', values: { active: active } });
        htmx.ajax('GET', '/agent/conversations', { target: '#conv-list-m', swap: 'innerHTML', values: { active: active } });
      },

      // 移动端会话抽屉
      toggleDrawer: function(open) {
        var drawer = document.getElementById('conv-drawer');
        if (!drawer) return;
        drawer.classList.toggle('hidden', !open);
        drawer.setAttribute('aria-hidden', open ? 'false' : 'true');
      },

      // 会话 "..." 菜单（重命名 / 删除）
      openMenu: function(event, id, btn) {
        event.stopPropagation();
        AppAgent.closeMenu();
        var menu = document.createElement('div');
        menu.className = 'conv-menu fixed z-50 bg-white dark:bg-gray-800 rounded-lg shadow-xl border border-gray-200 dark:border-gray-700 py-1 text-xs min-w-32';
        var rename = document.createElement('button');
        rename.className = 'block w-full text-left px-3 py-1.5 hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-700 dark:text-gray-200';
        rename.textContent = '重命名';
        rename.onclick = function(e) { e.stopPropagation(); AppAgent.closeMenu(); AppAgent.renameConversation(id); };
        var del = document.createElement('button');
        del.className = 'block w-full text-left px-3 py-1.5 hover:bg-gray-100 dark:hover:bg-gray-700 text-red-500';
        del.textContent = '删除';
        del.onclick = function(e) { e.stopPropagation(); AppAgent.closeMenu(); AppAgent.deleteConversation(id); };
        menu.appendChild(rename);
        menu.appendChild(del);
        document.body.appendChild(menu);
        var rect = btn.getBoundingClientRect();
        menu.style.top = Math.max(8, rect.bottom + 4) + 'px';
        menu.style.left = Math.min(rect.left, window.innerWidth - 140) + 'px';
        AppAgent._menu = menu;
        setTimeout(function() {
          document.addEventListener('click', AppAgent.closeMenu, { once: true });
        }, 0);
      },
      closeMenu: function() {
        if (AppAgent._menu) { AppAgent._menu.remove(); AppAgent._menu = null; }
      },

      // 重命名（内联编辑，PUT /api/conversations/{id}）
      renameConversation: function(id) {
        var item = document.querySelector('#conv-list [data-conv-id="' + id + '"] .conv-title')
                || document.querySelector('#conv-list-m [data-conv-id="' + id + '"] .conv-title');
        if (!item) return;
        var old = item.textContent;
        var input = document.createElement('input');
        input.value = old;
        input.className = 'w-full text-xs px-1 py-0.5 border border-indigo-400 rounded bg-white dark:bg-gray-900 text-gray-800 dark:text-gray-200 focus:outline-none';
        item.replaceWith(input);
        input.focus();
        input.select();
        var done = function() {
          var t = input.value.trim();
          if (t && t !== old) {
            fetch('/api/conversations/' + id, {
              method: 'PUT',
              credentials: 'same-origin',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify({ title: t })
            }).then(function(r) {
              if (r.ok) { AppAgent.refreshConvList(); }
              else { App.showToast('重命名失败', 'error'); AppAgent.refreshConvList(); }
            }).catch(function() { App.showToast('重命名失败', 'error'); AppAgent.refreshConvList(); });
          } else {
            AppAgent.refreshConvList();
          }
        };
        input.onblur = done;
        input.onkeydown = function(e) {
          if (e.key === 'Enter') { e.preventDefault(); input.blur(); }
          if (e.key === 'Escape') { input.value = old; input.blur(); }
        };
      },

      // 删除对话（DELETE /api/conversations/{id}）
      deleteConversation: function(id) {
        if (!confirm('删除该对话？')) return;
        fetch('/api/conversations/' + id, { method: 'DELETE', credentials: 'same-origin' })
          .then(function(r) {
            if (r.ok) {
              if (AppAgent.currentConvId === id) {
                AppAgent.currentConvId = null;
                document.getElementById('agent-conversation-id').value = '';
                var box = document.getElementById('agent-chat-messages');
                if (box) box.innerHTML = '';
              }
              AppAgent.refreshConvList();
            } else {
              App.showToast('删除失败', 'error');
            }
          })
          .catch(function() { App.showToast('删除失败', 'error'); });
      },

      // 提交后：刷新会话列表标题
      onSent: function() {
        AppAgent.refreshConvList();
      },

      // 发送消息：按流式开关走 SSE 流式 或 一次性 JSON
      send: function() {
        if (AppAgent.streaming) return;
        var input = document.getElementById('agent-input');
        var text = input.value.trim();
        if (!text) return;
        var hidden = document.getElementById('agent-conversation-id');
        var useStream = document.getElementById('agent-stream-toggle').checked;

        var doSend = function() {
          AppAgent.appendMessage('user', text);
          input.value = '';
          input.style.height = 'auto';
          var btn = document.getElementById('agent-send-btn');
          var finish = function() {
            AppAgent.streaming = false;
            btn.disabled = false;
            btn.textContent = '发送';
            AppAgent.onSent();
            document.getElementById('agent-input').focus();
          };

          if (useStream) {
            // ---- SSE 流式 ----
            var bubble = AppAgent.startStreamBubble();
            AppAgent.streaming = true;
            btn.disabled = true;
            btn.textContent = '生成中...';
            var finishOnce = function() {
              if (!AppAgent.streaming) return;
              AppAgent.streaming = false;
              bubble.finish();
              btn.disabled = false;
              btn.textContent = '发送';
              AppAgent.onSent();
              document.getElementById('agent-input').focus();
            };
            fetch('/api/agent/chat/stream', {
              method: 'POST',
              credentials: 'same-origin',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify({
                messages: [{ role: 'user', content: text }],
                model: document.getElementById('agent-model-hidden').value,
                conversation_id: AppAgent.currentConvId
              })
            }).then(function(r) {
              if (!r.ok) {
                return r.json().catch(function() { return {}; }).then(function(e) {
                  throw new Error(e.error || ('请求失败 (' + r.status + ')'));
                });
              }
              var reader = r.body.getReader();
              var decoder = new TextDecoder();
              var buffer = '';
              var read = function() {
                reader.read().then(function(res) {
                  if (res.done) { finishOnce(); return; }
                  buffer += decoder.decode(res.value, { stream: true });
                  var idx;
                  while ((idx = buffer.indexOf('\n\n')) >= 0) {
                    var frame = buffer.slice(0, idx);
                    buffer = buffer.slice(idx + 2);
                    frame.split('\n').forEach(function(line) {
                      if (line.indexOf('data:') !== 0) return;
                      var data = line.slice(5).trim();
                      if (!data) return;
                      if (data === '[DONE]') { finishOnce(); reader.cancel(); return; }
                      // 服务端 json_data 编码（多行 delta 安全）；旧格式纯文本兜底
                      try { var parsed = JSON.parse(data); bubble.append(parsed); }
                      catch (e) { bubble.append(data); }
                    });
                    if (!AppAgent.streaming) return;
                  }
                  if (AppAgent.streaming) read();
                }).catch(function(e) {
                  finishOnce();
                  App.showToast('流式中断: ' + e.message, 'error');
                });
              };
              read();
            }).catch(function(e) {
              finishOnce();
              App.showToast(e.message, 'error');
            });
          } else {
            // ---- 非流式（一次性 JSON）----
            AppAgent.streaming = true;
            btn.disabled = true;
            btn.textContent = '生成中...';
            fetch('/api/agent/chat', {
              method: 'POST',
              credentials: 'same-origin',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify({
                messages: [{ role: 'user', content: text }],
                model: document.getElementById('agent-model-hidden').value,
                conversation_id: AppAgent.currentConvId
              })
            }).then(function(r) {
              return r.json().catch(function() { return {}; }).then(function(data) {
                if (!r.ok) throw new Error(data.error || ('请求失败 (' + r.status + ')'));
                return data;
              });
            }).then(function(data) {
              if (data.conversation_id) {
                AppAgent.currentConvId = data.conversation_id;
                hidden.value = data.conversation_id;
              }
              AppAgent.appendMessage('assistant', data.reply || '（AI 未返回内容）', true);
            }).catch(function(e) {
              App.showToast(e.message, 'error');
            }).then(function() {
              finish();
            });
          }
        };

        if (AppAgent.currentConvId) {
          hidden.value = AppAgent.currentConvId;
          doSend();
        } else {
          // 首次发送：先创建对话再发送
          fetch('/api/conversations', { method: 'POST', credentials: 'same-origin' })
            .then(function(r) { return r.json(); })
            .then(function(conv) {
              AppAgent.currentConvId = conv.id;
              hidden.value = conv.id;
              doSend();
            })
            .catch(function() { App.showToast('创建对话失败', 'error'); });
        }
      }
    };

    // ====== document 级事件委托（CSP 收敛：data-* 属性统一入口，一次绑定避免重复） ======

    // 点击委托
    document.addEventListener('click', function(e) {
      var target = e.target;
      if (!target || !target.closest) return;

      // 模态框内容根：阻止点击冒泡（原 onclick="event.stopPropagation()"）
      if (target.closest('[data-modal-root]')) e.stopPropagation();

      // 模态框背景：仅点击背景自身时关闭（原 onclick="if(event.target===this)App.closeModal()"）
      if (target.hasAttribute('data-modal-backdrop')) { App.closeModal(); return; }

      // 关闭按钮
      if (target.closest('[data-close-modal]')) { App.closeModal(); return; }

      // 主题切换（独立页）
      if (target.closest('[data-toggle-dark]')) { toggleDark(); return; }

      // 登录/注册切换
      if (target.closest('[data-toggle-register]')) { toggleRegister(); return; }

      // 设置输入框值（上传目标目录重置为根）
      var setEl = target.closest('[data-set-value-target]');
      if (setEl) {
        var setTgt = document.getElementById(setEl.getAttribute('data-set-value-target'));
        if (setTgt) setTgt.value = setEl.getAttribute('data-set-value') || '';
        return;
      }

      // 触发文件选择器（上传文件夹）
      var clickTgt = target.closest('[data-click-target]');
      if (clickTgt) {
        var fileInput = document.getElementById(clickTgt.getAttribute('data-click-target'));
        if (fileInput) fileInput.click();
        return;
      }

      // 上级目录
      if (target.closest('[data-go-parent]')) { App.goParent(); return; }

      // 上传队列取消
      if (target.closest('[data-upload-cancel]')) { window.UploadQueue.cancel(); return; }

      // 会话 "..." 菜单
      var convMenuBtn = target.closest('[data-conv-menu]');
      if (convMenuBtn && window.AppAgent) {
        window.AppAgent.openMenu(e, convMenuBtn.getAttribute('data-conv-menu'), convMenuBtn);
        return;
      }

      // 批量删除 / 下载
      if (target.closest('[data-batch-delete]')) { batchDelete(); return; }
      if (target.closest('[data-batch-download]')) { batchDownload(); return; }

      // 复制 markdown 原文 / 文本
      if (target.closest('[data-copy-markdown]')) {
        var mdData = document.getElementById('markdown-data');
        if (mdData) { navigator.clipboard.writeText(JSON.parse(mdData.textContent)); App.showToast('已复制到剪贴板', 'info'); }
        return;
      }
      var copyTextBtn = target.closest('[data-copy-text]');
      if (copyTextBtn) {
        var pre = copyTextBtn.closest('.relative')?.querySelector('pre');
        if (pre) { navigator.clipboard.writeText(pre.textContent); App.showToast('已复制到剪贴板', 'info'); }
        return;
      }

      // 移动端会话抽屉开合
      var drawerBtn = target.closest('[data-agent-toggle-drawer]');
      if (drawerBtn && window.AppAgent) {
        window.AppAgent.toggleDrawer(drawerBtn.getAttribute('data-agent-toggle-drawer') === 'true');
        return;
      }

      // 新建对话
      if (target.closest('[data-agent-new-conversation]') && window.AppAgent) {
        window.AppAgent.newConversation();
        return;
      }

      // 阻止默认（链接列表编辑/删除按钮）
      if (target.closest('[data-prevent]')) e.preventDefault();

      // 会话条目点击：加载消息历史（菜单按钮除外）
      var convItem = target.closest('[data-conv-id]');
      if (convItem && !target.closest('.conv-menu-btn') && window.AppAgent) {
        window.AppAgent.loadConversation(parseInt(convItem.getAttribute('data-conv-id'), 10));
      }
    });

    // 提交委托：按表单 id 分发（各 handler 内部已 preventDefault）
    document.addEventListener('submit', function(e) {
      var form = e.target;
      if (!form || !form.id) return;
      if (form.id === 'settings-form') App.saveSettings(e);
      else if (form.id === 'upload-form') App.handleUploadForm(e);
      else if (form.id === 'pwd-modal-form') handlePwdModal(e);
      else if (form.id === 'login-form') handleLogin(e);
      else if (form.id === 'pwd-form') handleChangePwd(e);
      else if (form.id === 'agent-chat-form') { e.preventDefault(); window.AppAgent.send(); }
    });

    // change 委托
    document.addEventListener('change', function(e) {
      var el = e.target;
      if (!el) return;
      if (el.id === 'select-all') { toggleSelectAll(el); return; }
      if (el.classList && el.classList.contains('file-checkbox')) { updateBatchToolbar(); return; }
      if (el.id === 'upload-folder-input') { App.handleFolderInput(el); return; }
      if (el.id === 'agent-model-select') {
        var hidden = document.getElementById('agent-model-hidden');
        if (hidden) hidden.value = el.value;
        return;
      }
      if (el.id === 'global-search') {
        var pathEl = document.getElementById('drive-current-path');
        if (!pathEl) return;
        var searchEl = document.querySelector('input[name="search"]');
        if (el.checked) {
          pathEl.dataset.savedPath = pathEl.value || '/';
          pathEl.value = '';
          if (searchEl && searchEl.value.trim()) {
            htmx.ajax('GET', '/drive/list', { target: '#drive-file-list', values: { path: '', search: searchEl.value.trim() } });
          }
        } else {
          pathEl.value = pathEl.dataset.savedPath || '/';
          htmx.ajax('GET', '/drive/list', { target: '#drive-file-list', values: { path: pathEl.value } });
          htmx.ajax('GET', '/drive/breadcrumbs', { target: '#drive-breadcrumbs', values: { path: pathEl.value } });
        }
        return;
      }
    });

    // input 委托：range 标签 / agent 输入框自适应高度
    document.addEventListener('input', function(e) {
      var el = e.target;
      if (!el) return;
      if (el.hasAttribute && el.hasAttribute('data-range-label')) {
        el.previousElementSibling.textContent = 'Temperature (' + el.value + ')';
        return;
      }
      if (el.id === 'agent-input') {
        el.style.height = 'auto';
        el.style.height = Math.min(el.scrollHeight, 150) + 'px';
        return;
      }
    });

    // keydown 委托：agent 输入框 Enter 发送（Shift+Enter 换行）
    document.addEventListener('keydown', function(e) {
      if (e.key === 'Enter' && !e.shiftKey && e.target && e.target.hasAttribute && e.target.hasAttribute('data-agent-enter-submit')) {
        e.preventDefault();
        if (e.target.form) e.target.form.requestSubmit();
      }
    });

    // 媒体加载失败：error 事件不冒泡，捕获阶段在 window 上委托（原 onerror="showMediaError(this)"）
    window.addEventListener('error', function(e) {
      var el = e.target;
      if (el && el.hasAttribute && el.hasAttribute('data-media-el')) App.showMediaError(el);
    }, true);

    // 会话菜单：滚动时关闭
    document.addEventListener('scroll', function() { window.AppAgent.closeMenu(); }, true);

    // 拖拽上传覆盖层（drive.html 原 ondragover/ondragleave/ondrop）
    document.addEventListener('dragover', function(e) {
      var target = e.target;
      if (!target || !target.closest || !target.closest('[data-drop-zone]')) return;
      e.preventDefault();
      var overlay = document.getElementById('drop-overlay');
      if (overlay) overlay.style.display = 'flex';
    });
    document.addEventListener('dragleave', function(e) {
      var target = e.target;
      if (!target || !target.closest) return;
      var zone = target.closest('[data-drop-zone]');
      if (zone && target === zone) {
        var overlay = document.getElementById('drop-overlay');
        if (overlay) overlay.style.display = 'none';
      }
    });
    document.addEventListener('drop', function(e) {
      var target = e.target;
      if (!target || !target.closest || !target.closest('[data-drop-zone]')) return;
      e.preventDefault();
      var overlay = document.getElementById('drop-overlay');
      if (overlay) overlay.style.display = 'none';
      App.handleDrop(e);
    });

    // 模态框片段载入后初始化（new_folder_form 目标目录 / preview markdown / 视频续播）
    document.body.addEventListener('htmx:afterSwap', function(e) {
      var t = e.detail.target;
      if (t && t.id === 'modal-container') App.initFragment(t);
    });

