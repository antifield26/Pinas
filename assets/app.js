const App = {
    state: {
        currentPath: new URLSearchParams(window.location.search).get('path') || "",
        username: localStorage.getItem('cloud_username') || "",
        role: localStorage.getItem('cloud_role') || "",
        authMode: "login",
        currentView: "home",
        files: [],
        selectedFileNames: new Set(),
        currentEditingFile: "",
        isPreviewMode: false,
        shares: [],
        currentShareItem: null,
        systemStatusInterval: null,
        links: [],
    },
    constants: {
        VIDEO_EXTS: ['mp4', 'webm', 'ogg', 'm4v', 'avi', 'mov', 'mkv', 'flv', 'wmv', '3gp', 'ts', 'mts'],
        AUDIO_EXTS: ['mp3', 'wav', 'ogg', 'aac', 'flac', 'm4a', 'opus', 'wma', 'amr', 'ape'],
        TEXT_EXTS: ['txt', 'md', 'markdown', 'json', 'ini', 'conf', 'rs', 'toml', 'js', 'html', 'css', 'yaml', 'yml', 'xml', 'log', 'cfg'],
        IMAGE_EXTS: ['jpg', 'jpeg', 'png', 'gif', 'webp', 'bmp', 'tiff', 'ico', 'heic']
    },
    utils: {
        escapeHtml(text) {
            if (!text) return '';
            return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&#039;");
        },
        toast(message, type = 'info') {
            const container = document.getElementById('toast-container');
            const toast = document.createElement('div');
            toast.className = `px-4 py-3 rounded-xl shadow-lg text-sm font-medium text-white transition-all duration-300 transform translate-y-2 opacity-0 flex items-center gap-2 pointer-events-auto`;
            const bgColors = { success: 'bg-green-600', error: 'bg-red-600', info: 'bg-indigo-600', warning: 'bg-amber-500' };
            toast.classList.add(bgColors[type] || bgColors.info);
            toast.innerText = message;
            container.appendChild(toast);
            setTimeout(() => toast.classList.remove('translate-y-2', 'opacity-0'), 10);
            setTimeout(() => {
                toast.classList.add('opacity-0', 'translate-y-[-10px]');
                setTimeout(() => toast.remove(), 300);
            }, 3500);
        }
    },
    api: {
        getAuthHeaders(contentType = null) {
            const token = localStorage.getItem('cloud_auth_token');
            const headers = {};
            if (contentType) headers['Content-Type'] = contentType;
            if (token) headers['Authorization'] = `Bearer ${token}`;
            return headers;
        },
        async request(url, options = {}) {
            options.headers = { ...options.headers, ...this.getAuthHeaders() };
            try {
                const response = await fetch(url, options);
                if (response.status === 401) {
                    document.getElementById('login-overlay').classList.remove('hidden');
                    throw new Error('Unauthorized');
                }
                return response;
            } catch (err) {
                if (err.message !== 'Unauthorized') App.utils.toast('网络请求连接失败', 'error');
                throw err;
            }
        }
    },
    view: {
        renderAll() {
            this.renderBreadcrumbs();
            this.renderFileList();
            this.renderUserBadge();
        },
        renderUserBadge() {
            const badge = document.getElementById('user-badge');
            if (App.state.username) {
                badge.innerText = `${App.state.username} (${App.state.role === 'admin' ? '管理员' : '专享空间'})`;
                badge.classList.remove('hidden');
            } else {
                badge.classList.add('hidden');
            }
        },
        toggleAuthMode() {
            const errTip = document.getElementById('login-error');
            errTip.classList.add('hidden');
            if (App.state.authMode === "login") {
                App.state.authMode = "register";
                document.getElementById('auth-title').innerText = "🔑 注册账号";
                document.getElementById('auth-subtitle').innerText = "注册后系统将自动为您分配独立的网盘存储空间";
                document.getElementById('auth-submit-btn').innerText = "立即注册并接入";
                document.getElementById('auth-toggle-link').innerText = "已有账号？返回密码登录";
            } else {
                App.state.authMode = "login";
                document.getElementById('auth-title').innerText = "💾 Private Cloud";
                document.getElementById('auth-subtitle').innerText = "该网盘已受安全保护，请输入访问密码";
                document.getElementById('auth-submit-btn').innerText = "验证并进入";
                document.getElementById('auth-toggle-link').innerText = "没有账号？点击注册";
            }
        },
        renderBreadcrumbs() {
            const bcContainer = document.getElementById('path-breadcrumbs');
            if (!App.state.currentPath) {
                bcContainer.innerHTML = `<span class="text-gray-400">/</span>`;
                return;
            }
            const parts = App.state.currentPath.split('/');
            let accumulated = '';
            let html = `<span class="cursor-pointer text-indigo-600 hover:underline font-bold" onclick="App.actions.navigateTo('')">Root</span>`;
            parts.forEach((p, idx) => {
                accumulated += (idx === 0 ? p : '/' + p);
                html += ` <span class="text-gray-300">/</span> <span class="cursor-pointer text-indigo-600 hover:underline" onclick="App.actions.navigateTo('${encodeURIComponent(accumulated)}')">${App.utils.escapeHtml(p)}</span>`;
            });
            bcContainer.innerHTML = html;
        },
        renderFileList() {
            const tbody = document.getElementById('file-list-body');
            if (App.state.files.length === 0) {
                tbody.innerHTML = `<tr><td colspan="4" class="p-8 text-center text-gray-400">当前目录下空空如也...</td></tr>`;
                document.getElementById('selectAllCheckbox').checked = false;
                return;
            }
            let htmlContent = App.state.files.map(item => {
                const safeName = App.utils.escapeHtml(item.name);
                const encodedName = encodeURIComponent(item.name);
                const fullItemPath = App.state.currentPath ? `${App.state.currentPath}/${item.name}` : item.name;
                const pathSegments = fullItemPath.split('/').map(encodeURIComponent).join('/');
                const downloadPath = App.state.username ? `${encodeURIComponent(App.state.username)}/${pathSegments}` : pathSegments;
                const fileUrl = `/downloads/${downloadPath}`;
                if (item.is_dir) {
                    const encodedFolderDir = encodeURIComponent(fullItemPath);
                    return `
                    <tr class="hover:bg-gray-50/80 transition cursor-pointer" onclick="App.actions.navigateTo('${encodedFolderDir}')">
                        <td class="p-4" onclick="event.stopPropagation()"><input type="checkbox" class="item-checkbox rounded text-indigo-600 focus:ring-indigo-500" data-raw="${encodedName}" onchange="App.actions.handleSelectRow(this, '${encodedName}')"></td>
                        <td class="p-4 font-medium text-gray-900 flex items-center select-none">📁 <span class="ml-2 hover:text-indigo-600 hover:underline">${safeName}</span></td>
                        <td class="p-4 text-gray-400 text-xs">-</td>
                        <td class="p-4 text-right space-x-2" onclick="event.stopPropagation()">
                            <button onclick="App.actions.openCreateShare('${fullItemPath}', true)" class="text-xs font-medium text-green-600 hover:text-green-900 transition">🔗 分享</button>
                            <button onclick="App.actions.moveFile('${encodedName}')" class="text-xs font-medium text-indigo-600 hover:text-indigo-900 transition">移动</button>
                            <button onclick="App.actions.renameFile('${encodedName}')" class="text-xs font-medium text-amber-600 hover:text-amber-900 transition">重命名</button>
                            <button onclick="App.actions.deleteFile('${encodedName}')" class="text-xs font-medium text-red-600 hover:text-red-900 transition">删除</button>
                        </td>
                    </tr>`;
                } else {
                    const ext = item.name.split('.').pop().toLowerCase();
                    const isText = App.constants.TEXT_EXTS.includes(ext);
                    const imageExts = ['jpg', 'jpeg', 'png', 'gif', 'webp', 'bmp', 'svg', 'ico'];
                    const pdfExt = 'pdf';
                    let previewBtn = '', actionBtn = '';
                    if (imageExts.includes(ext) || ext === pdfExt) {
                        previewBtn = `<button onclick="App.actions.playMedia('${fullItemPath}', '${ext === pdfExt ? 'pdf' : 'image'}', '${encodedName}')" class="text-xs font-medium text-indigo-600 hover:text-indigo-900 transition">👁️ 预览</button>`;
                    }
                    if (App.constants.VIDEO_EXTS.includes(ext)) {
                        actionBtn = `<button onclick="App.actions.playMedia('${fullItemPath}', 'video', '${encodedName}')" class="text-xs font-medium text-green-600 hover:text-green-900 transition">▶️ 播放</button>`;
                    } else if (App.constants.AUDIO_EXTS.includes(ext)) {
                        actionBtn = `<button onclick="App.actions.playMedia('${fullItemPath}', 'audio', '${encodedName}')" class="text-xs font-medium text-purple-600 hover:text-purple-900 transition">🎵 播放</button>`;
                    }
                    return `
                    <tr class="hover:bg-gray-50/80 transition">
                        <td class="p-4"><input type="checkbox" class="item-checkbox rounded text-indigo-600 focus:ring-indigo-500" data-raw="${encodedName}" onchange="App.actions.handleSelectRow(this, '${encodedName}')"></td>
                        <td class="p-4 font-medium text-gray-800 max-w-xs truncate ${isText ? 'cursor-pointer text-indigo-600 hover:text-indigo-900 hover:underline select-none' : 'select-none'}" title="${safeName}" ${isText ? `onclick="App.actions.openEditor('${encodedName}')"` : ''}>
                            ${isText ? '📝' : '📄'} ${safeName}
                        </td>
                        <td class="p-4 text-gray-500 text-xs">${item.size_mb} MB</td>
                        <td class="p-4 text-right space-x-3">
                            ${previewBtn}
                            ${actionBtn}
                            <button onclick="App.actions.openCreateShare('${fullItemPath}', false)" class="text-xs font-medium text-green-600 hover:text-green-900 transition">🔗 分享</button>
                            ${isText ? `<button onclick="App.actions.openEditor('${encodedName}')" class="text-xs font-medium text-indigo-600 hover:text-indigo-900 transition">编辑</button>` : ''}
                            <button onclick="App.actions.downloadFile('${fileUrl}', '${safeName}')" class="text-xs font-medium text-blue-600 hover:text-blue-900 transition">下载</button>
                            <button onclick="App.actions.moveFile('${encodedName}')" class="text-xs font-medium text-indigo-600 hover:text-indigo-900 transition">移动</button>
                            <button onclick="App.actions.renameFile('${encodedName}')" class="text-xs font-medium text-amber-600 hover:text-amber-900 transition">重命名</button>
                            <button onclick="App.actions.deleteFile('${encodedName}')" class="text-xs font-medium text-red-600 hover:text-red-900 transition">删除</button>
                        </td>
                    </tr>`;
                }
            }).join('');
            tbody.innerHTML = htmlContent;
            const checkboxes = tbody.querySelectorAll('.item-checkbox');
            const allChecked = checkboxes.length > 0 && Array.from(checkboxes).every(c => c.checked);
            document.getElementById('selectAllCheckbox').checked = allChecked;
        },
        renderSharesList() {
            const container = document.getElementById('share-list-container');
            if (!App.state.shares.length) {
                container.innerHTML = '<div class="text-center text-gray-400 py-10">暂无任何分享链接，请先在文件列表中创建分享。</div>';
                return;
            }
            let html = '<div class="space-y-3">';
            for (const share of App.state.shares) {
                const shareUrl = `${window.location.origin}/s/${share.code}`;
                const expires = share.expires_at ? new Date(share.expires_at).toLocaleString() : '永久有效';
                const pwdStatus = share.has_password ? '🔒 有密码' : '🔓 无密码';
                html += `
                    <div class="border border-gray-200 rounded-xl p-4 bg-gray-50/30 hover:bg-white transition">
                        <div class="flex flex-wrap justify-between items-start gap-2">
                            <div class="flex-1 min-w-0">
                                <div class="font-mono text-xs text-indigo-600 break-all">${App.utils.escapeHtml(share.code)}</div>
                                <div class="text-sm font-medium text-gray-800 mt-1">📄 ${App.utils.escapeHtml(share.file_path)}</div>
                                <div class="flex flex-wrap gap-3 text-xs text-gray-500 mt-2">
                                    <span>⏱️ ${expires}</span>
                                    <span>${pwdStatus}</span>
                                    <span>📊 下载次数: ${share.download_count || 0}</span>
                                </div>
                            </div>
                            <div class="flex gap-2">
                                <button onclick="App.actions.copyShareUrl('${shareUrl}')" class="px-3 py-1 text-xs bg-gray-200 hover:bg-gray-300 rounded-lg transition">复制链接</button>
                                <button onclick="App.actions.deleteShare('${share.code}')" class="px-3 py-1 text-xs bg-red-100 hover:bg-red-200 text-red-700 rounded-lg transition">删除</button>
                            </div>
                        </div>
                    </div>
                `;
            }
            html += '</div>';
            container.innerHTML = html;
        },
        openPlayer(url, type, encodedName) {
            const name = decodeURIComponent(encodedName);
            document.getElementById('player-title').innerText = `查看: ${name}`;
            const contentZone = document.getElementById('player-content');
            const ext = name.split('.').pop().toLowerCase();
            const imageExts = ['jpg', 'jpeg', 'png', 'gif', 'webp', 'bmp', 'svg', 'ico'];
            const pdfExt = 'pdf';

            if (imageExts.includes(ext)) {
                contentZone.innerHTML = `<img src="${url}" class="max-w-full max-h-[70vh] object-contain rounded-lg shadow-lg" alt="${name}">`;
            } else if (ext === pdfExt) {
                contentZone.innerHTML = `<iframe src="${url}" class="w-full h-[80vh] border-0 rounded-lg"></iframe>`;
            } else if (type === 'video') {
                contentZone.innerHTML = `<video src="${url}" controls autoplay class="w-full max-h-[60vh] object-contain"></video>`;
            } else if (type === 'audio') {
                contentZone.innerHTML = `<audio src="${url}" controls autoplay class="w-full py-4"></audio>`;
            } else {
                contentZone.innerHTML = `<p class="text-white">不支持的媒体类型</p>`;
            }
            document.getElementById('player-modal').classList.remove('hidden');
        },
        closePlayer() {
            const contentZone = document.getElementById('player-content');
            const video = contentZone.querySelector('video');
            const audio = contentZone.querySelector('audio');
            if (video) { video.pause(); video.src = ''; }
            if (audio) { audio.pause(); audio.src = ''; }
            contentZone.innerHTML = "";
            document.getElementById('player-modal').classList.add('hidden');
        },
        toggleEditorMode() {
            const textarea = document.getElementById('editor-textarea');
            const preview = document.getElementById('editor-preview');
            const btn = document.getElementById('btn-preview-toggle');
            App.state.isPreviewMode = !App.state.isPreviewMode;
            if (App.state.isPreviewMode) {
                const raw = textarea.value;
                const cleanHtml = DOMPurify.sanitize(marked.parse(raw));
                preview.innerHTML = cleanHtml;
                textarea.classList.add('hidden');
                preview.classList.remove('hidden');
                btn.innerText = '编辑';
            } else {
                preview.classList.add('hidden');
                textarea.classList.remove('hidden');
                btn.innerText = '预览';
            }
        },
        closeEditor() {
            document.getElementById('editor-modal').classList.add('hidden');
            App.state.currentEditingFile = "";
        }
    },
    actions: {
        // ========== 链接库相关方法 ==========
        async fetchLinks() {
            try {
                const res = await App.api.request('/api/links');
                if (res.ok) {
                    App.state.links = await res.json();
                    this.renderLinks();
                }
            } catch(e) {
                // 忽略错误
            }
        },

        renderLinks() {
            const container = document.getElementById('links-container');
            if (!container) return;
            if (!App.state.links.length) {
                container.innerHTML = '<div class="text-center text-gray-400 col-span-full py-8">暂无链接，点击“添加链接”创建</div>';
                return;
            }
            let html = '';
            for (const link of App.state.links) {
                html += `
                    <div class="bg-gray-50 rounded-lg p-3 flex items-center justify-between group hover:shadow transition">
                        <a href="${App.utils.escapeHtml(link.url)}" target="_blank" rel="noopener noreferrer" class="flex items-center gap-2 flex-1 min-w-0">
                            ${link.icon ? `<span class="text-xl">${App.utils.escapeHtml(link.icon)}</span>` : '<span class="text-xl">🔗</span>'}
                            <span class="text-sm font-medium text-gray-800 truncate">${App.utils.escapeHtml(link.title)}</span>
                        </a>
                        <div class="flex gap-1 opacity-0 group-hover:opacity-100 transition">
                            <button onclick="App.actions.openEditLinkModal(${link.id})" class="text-xs text-blue-600 hover:text-blue-800 p-1">✏️</button>
                            <button onclick="App.actions.deleteLink(${link.id})" class="text-xs text-red-600 hover:text-red-800 p-1">🗑️</button>
                        </div>
                    </div>
                `;
            }
            container.innerHTML = html;
        },

        openAddLinkModal() {
            const title = prompt('请输入链接标题：');
            if (!title) return;
            const url = prompt('请输入链接URL（以http://或https://开头）：');
            if (!url) return;
            const icon = prompt('请输入图标（可选，例如📁、🔗、🌐）', '🔗');
            this.createLink(title, url, icon);
        },

        async createLink(title, url, icon) {
            try {
                const res = await App.api.request('/api/links', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ title, url, icon: icon || null })
                });
                if (res.ok) {
                    App.utils.toast('链接添加成功', 'success');
                    await this.fetchLinks();
                } else {
                    const err = await res.text();
                    App.utils.toast(`添加失败: ${err}`, 'error');
                }
            } catch(e) {
                App.utils.toast('网络错误', 'error');
            }
        },

        openEditLinkModal(id) {
            const link = App.state.links.find(l => l.id === id);
            if (!link) return;
            const newTitle = prompt('修改标题', link.title);
            if (newTitle === null) return;
            const newUrl = prompt('修改URL', link.url);
            if (newUrl === null) return;
            const newIcon = prompt('修改图标（可选）', link.icon || '🔗');
            this.updateLink(id, newTitle, newUrl, newIcon);
        },

        async updateLink(id, title, url, icon) {
            try {
                const res = await App.api.request(`/api/links/${id}`, {
                    method: 'PUT',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ title, url, icon: icon || null })
                });
                if (res.ok) {
                    App.utils.toast('链接更新成功', 'success');
                    await this.fetchLinks();
                } else {
                    const err = await res.text();
                    App.utils.toast(`更新失败: ${err}`, 'error');
                }
            } catch(e) {
                App.utils.toast('网络错误', 'error');
            }
        },

        async deleteLink(id) {
            if (!confirm('确定要删除这个链接吗？')) return;
            try {
                const res = await App.api.request(`/api/links/${id}`, { method: 'DELETE' });
                if (res.ok) {
                    App.utils.toast('链接已删除', 'success');
                    await this.fetchLinks();
                } else {
                    const err = await res.text();
                    App.utils.toast(`删除失败: ${err}`, 'error');
                }
            } catch(e) {
                App.utils.toast('网络错误', 'error');
            }
        },
        switchView(viewName) {
            App.state.currentView = viewName;
            const homeView = document.getElementById('view-home');
            const driveView = document.getElementById('view-drive');
            const trashView = document.getElementById('view-trash');
            const userAdminView = document.getElementById('view-user-admin');

            // 隐藏所有视图
            homeView.classList.add('hidden');
            driveView.classList.add('hidden');
            if (trashView) trashView.classList.add('hidden');
            if (userAdminView) userAdminView.classList.add('hidden');

            if (viewName === 'home') {
                homeView.classList.remove('hidden');
                this.updateLayoutByRole(); // 刷新卡片可见性
                this.fetchLinks();   // 刷新链接库
            } else if (viewName === 'drive') {
                driveView.classList.remove('hidden');
                this.fetchFiles();
                this.fetchQuota();
            } else if (viewName === 'trash') {
                if (trashView) trashView.classList.remove('hidden');
                this.loadTrashList();
            } else if (viewName === 'user_admin') {
                if (userAdminView) userAdminView.classList.remove('hidden');
                this.loadUserList();
            }
        },
        async fetchFiles() {
            const keyword = document.getElementById('searchKeyword').value.trim();
            const sortBy = document.getElementById('sortBy').value;
            const url = `/api/files/list?path=${encodeURIComponent(App.state.currentPath)}&search=${encodeURIComponent(keyword)}&sort_by=${sortBy}`;
            try {
                const res = await App.api.request(url);
                if (res.ok) {
                    App.state.files = await res.json();
                    App.view.renderAll();
                    this.fetchQuota();
                }
            } catch (e) {}
        },
        async fetchQuota() {
            try {
                const res = await App.api.request('/api/admin/quota');
                if (res.ok) {
                    const data = await res.json();
                    document.getElementById('quota-used').innerText = data.used_mb;
                    document.getElementById('quota-total').innerText = data.quota_mb;
                    const percent = (data.used_mb / data.quota_mb) * 100;
                    document.getElementById('quota-bar').style.width = `${Math.min(percent, 100)}%`;
                    if (percent > 85) {
                        document.getElementById('quota-bar').classList.add('bg-red-500');
                        document.getElementById('quota-bar').classList.remove('bg-indigo-600');
                    } else if (percent > 70) {
                        document.getElementById('quota-bar').classList.add('bg-amber-500');
                        document.getElementById('quota-bar').classList.remove('bg-indigo-600');
                    } else {
                        document.getElementById('quota-bar').classList.add('bg-indigo-600');
                        document.getElementById('quota-bar').classList.remove('bg-red-500', 'bg-amber-500');
                    }
                }
            } catch(e) {
                console.error('获取配额失败', e);
            }
        },
        refreshList() { this.fetchFiles(); App.utils.toast('列表已同步刷新'); },
        navigateTo(encodedPath) {
            App.state.currentPath = decodeURIComponent(encodedPath);
            const newUrl = window.location.protocol + "//" + window.location.host + window.location.pathname + '?path=' + encodedPath;
            window.history.pushState({ path: App.state.currentPath }, '', newUrl);
            App.state.selectedFileNames.clear();
            this.fetchFiles();
        },
        handleSearch() { this.fetchFiles(); },
        handleSort() { this.fetchFiles(); },
        handleSelectRow(checkbox, encodedName) {
            let name;
            if (encodedName) {
                name = decodeURIComponent(encodedName);
            } else {
                const raw = checkbox.getAttribute('data-raw');
                if (!raw) return;
                name = decodeURIComponent(raw);
            }
            if (checkbox.checked) App.state.selectedFileNames.add(name);
            else App.state.selectedFileNames.delete(name);
            const checkboxes = document.querySelectorAll('#file-list-body .item-checkbox');
            const allChecked = checkboxes.length > 0 && Array.from(checkboxes).every(cb => cb.checked);
            document.getElementById('selectAllCheckbox').checked = allChecked;
        },
        toggleSelectAll(masterCheckbox) {
            const checkboxes = document.querySelectorAll('#file-list-body .item-checkbox');
            checkboxes.forEach(cb => {
                cb.checked = masterCheckbox.checked;
                cb.dispatchEvent(new Event('change'));
            });
        },
        updateLayoutByRole() {
            const isAdmin = App.state.role === 'admin';
            const statusCard = document.getElementById('system-monitor-card');
            if (statusCard) {
                statusCard.classList.toggle('hidden', !isAdmin);
            }
            const adminCard = document.getElementById('admin-user-card');
            if (adminCard) {
                adminCard.classList.toggle('hidden', !isAdmin);
            }
        },
        async submitAuth() {
            const username = document.getElementById('login-username').value.trim();
            const password = document.getElementById('login-password').value;
            const errTip = document.getElementById('login-error');
            if (!username || !password) return App.utils.toast('请输入用户名和密码', 'warning');
            const targetApi = App.state.authMode === "register" ? '/api/register' : '/api/login';
            try {
                const response = await fetch(targetApi, {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ username, password })
                });
                if (response.ok) {
                    const data = await response.json();
                    App.state.username = username;
                    App.state.role = data.role;
                    localStorage.setItem('cloud_username', username);
                    localStorage.setItem('cloud_auth_token', data.token);
                    localStorage.setItem('cloud_role', data.role);
                    document.getElementById('login-overlay').classList.add('hidden');
                    errTip.classList.add('hidden');
                    document.getElementById('login-password').value = '';
                    App.utils.toast(App.state.authMode === "register" ? '账号注册成功并切入控制台' : '认证成功，欢迎回来', 'success');
                    App.state.authMode = "login";
                    this.updateLayoutByRole();
                    this.startSystemStatusPolling();
                    this.switchView('home');
                } else {
                    const errMsg = await response.text();
                    errTip.innerText = App.state.authMode === "register" ? `❌ 注册失败: ${errMsg || '用户名已存在'}` : '❌ 用户名或密码错误';
                    errTip.classList.remove('hidden');
                }
            } catch (e) { App.utils.toast('网关认证请求异常', 'error'); }
        },
        async logout() {
            if (confirm("确定要退出登录吗？")) {
                try { await App.api.request('/api/logout', { method: 'POST' }); } catch(e){}
                localStorage.removeItem('cloud_auth_token');
                localStorage.removeItem('cloud_username');
                localStorage.removeItem('cloud_role');
                App.state.username = "";
                App.state.role = "";
                App.state.selectedFileNames.clear();
                this.stopSystemStatusPolling();
                document.getElementById('login-overlay').classList.remove('hidden');
                App.utils.toast('已安全切断本次会话', 'info');
                this.switchView('home');
            }
        },
        async createFolder() {
            const input = document.getElementById('newFolderName');
            const name = input.value.trim();
            if (!name) return App.utils.toast('目录名称不能为空', 'warning');
            try {
                const res = await App.api.request('/api/files/create_folder', {
                    method: 'POST', headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ name, current_path: App.state.currentPath })
                });
                if (res.ok) {
                    App.utils.toast(`文件夹 [${name}] 创建成功`, 'success');
                    input.value = '';
                    this.fetchFiles();
                } else {
                    const err = await res.text();
                    App.utils.toast(`新建失败: ${err}`, 'error');
                }
            } catch(e){}
        },
        async deleteFile(encodedName) {
            const name = decodeURIComponent(encodedName);
            if (!confirm(`确定删除 [${name}] 吗？文件将移至回收站`)) return;
            try {
                const res = await App.api.request('/api/files/delete', {
                    method: 'POST', headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ name, current_path: App.state.currentPath })
                });
                if (res.ok) {
                    App.utils.toast('已移至回收站', 'success');
                    App.state.selectedFileNames.delete(name);
                    this.fetchFiles();
                } else {
                    const err = await res.text();
                    App.utils.toast(`删除失败: ${err}`, 'error');
                }
            } catch(e){}
        },
        async renameFile(encodedName) {
            const oldName = decodeURIComponent(encodedName);
            const newName = prompt(`请输入 [${oldName}] 的新名称:`, oldName);
            if (!newName || newName.trim() === oldName) return;
            try {
                const res = await App.api.request('/api/files/rename', {
                    method: 'POST', headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ name: oldName, new_name: newName.trim(), current_path: App.state.currentPath })
                });
                if (res.ok) {
                    App.utils.toast('重命名成功', 'success');
                    this.fetchFiles();
                } else {
                    const err = await res.text();
                    App.utils.toast(`更名失败: ${err}`, 'error');
                }
            } catch(e){}
        },
        async moveFile(encodedName) {
            const name = decodeURIComponent(encodedName);
            const targetPath = prompt(`请输入项目 [${name}] 要移动到的目标父目录路径 (留空代表根目录 /):`, App.state.currentPath);
            if (targetPath === null) return;
            try {
                const res = await App.api.request('/api/files/move', {
                    method: 'POST', headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ name, current_path: App.state.currentPath, target_dir: targetPath.trim() })
                });
                if (res.ok) {
                    App.utils.toast(`成功移动项目`, 'success');
                    App.state.selectedFileNames.delete(name);
                    this.fetchFiles();
                } else {
                    const err = await res.text();
                    App.utils.toast(`移动失败: ${err}`, 'error');
                }
            } catch(e){}
        },
        async moveSelected() {
            if (App.state.selectedFileNames.size === 0) return App.utils.toast('请先勾选需要批量移动的项目', 'warning');
            const targetPath = prompt(`请输入已选中 (${App.state.selectedFileNames.size}) 个项目要移动到的目标父目录路径 (留空代表根目录):`, App.state.currentPath);
            if (targetPath === null) return;
            try {
                const res = await App.api.request('/api/move_batch', {
                    method: 'POST', headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ names: Array.from(App.state.selectedFileNames), current_path: App.state.currentPath, target_path: targetPath.trim() })
                });
                if (res.ok) {
                    App.utils.toast('批量文件移动任务执行成功', 'success');
                    App.state.selectedFileNames.clear();
                    this.fetchFiles();
                } else {
                    const err = await res.text();
                    App.utils.toast(`批量移动失败: ${err}`, 'error');
                }
            } catch(e){}
        },
        async openEditor(encodedName) {
            const name = decodeURIComponent(encodedName);
            App.state.currentEditingFile = name;
            document.getElementById('editor-title').innerText = `编辑器 ⟴ ${name}`;
            const filePath = App.state.currentPath ? `${App.state.currentPath}/${name}` : name;
            App.state.isPreviewMode = false;
            const textarea = document.getElementById('editor-textarea');
            const preview = document.getElementById('editor-preview');
            const btn = document.getElementById('btn-preview-toggle');
            textarea.classList.remove('hidden');
            preview.classList.add('hidden');
            btn.innerText = '预览';
            try {
                const res = await App.api.request(`/api/edit/get?path=${encodeURIComponent(filePath)}`);
                if (res.ok) {
                    document.getElementById('editor-textarea').value = await res.text();
                    document.getElementById('editor-modal').classList.remove('hidden');
                } else {
                    App.utils.toast('无法读取文本流内容', 'error');
                }
            } catch(e){}
        },
        async saveFileContent() {
            const filePath = App.state.currentPath ? `${App.state.currentPath}/${App.state.currentEditingFile}` : App.state.currentEditingFile;
            const content = document.getElementById('editor-textarea').value;
            try {
                const res = await App.api.request('/api/edit/save', {
                    method: 'POST', headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ path: filePath, content })
                });
                if (res.ok) {
                    App.utils.toast('数据保存成功！磁盘已同步更新', 'success');
                    this.fetchFiles();
                } else {
                    const errMsg = await res.text();
                    App.utils.toast(`保存失败: ${errMsg}`, 'error');
                }
            } catch (e) { App.utils.toast('网络保存请求触发系统中断', 'error'); }
        },
        async downloadSelectedZip() {
            if (App.state.selectedFileNames.size === 0) return App.utils.toast('请先勾选需要打包的文件', 'warning');
            try {
                App.utils.toast('正在打包远端资源，请稍候...', 'info');
                const res = await App.api.request('/api/files/download_zip', {
                    method: 'POST', headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ names: Array.from(App.state.selectedFileNames), current_path: App.state.currentPath })
                });
                if (res.ok) {
                    const blob = await res.blob();
                    const url = window.URL.createObjectURL(blob);
                    const a = document.createElement('a');
                    a.href = url;
                    a.download = `archive_${new Date().getTime()}.zip`;
                    document.body.appendChild(a);
                    a.click();
                    a.remove();
                    window.URL.revokeObjectURL(url);
                    App.utils.toast('打包下载完毕', 'success');
                } else {
                    const err = await res.text();
                    App.utils.toast(`打包失败: ${err}`, 'error');
                }
            } catch(e){}
        },
        async downloadFile(url, filename) {
            try {
                const res = await App.api.request(url, { method: 'GET' });
                if (res.ok) {
                    const blob = await res.blob();
                    const blobUrl = window.URL.createObjectURL(blob);
                    const a = document.createElement('a');
                    a.href = blobUrl;
                    a.download = filename;
                    document.body.appendChild(a);
                    a.click();
                    a.remove();
                    window.URL.revokeObjectURL(blobUrl);
                } else {
                    App.utils.toast('下载失败', 'error');
                }
            } catch (e) { App.utils.toast('下载请求异常', 'error'); }
        },
    async playMedia(fullItemPath, type, encodedName) {
        const token = localStorage.getItem('cloud_auth_token');
        if (!token) {
            App.utils.toast('请先登录', 'error');
            return;
        }
        // 将 token 作为查询参数附加到 URL
        const mediaUrl = `/api/media/${encodeURIComponent(fullItemPath)}?token=${encodeURIComponent(token)}`;
        App.view.openPlayer(mediaUrl, type, encodedName);
    },
        async uploadFile() {
            const fileInput = document.getElementById('fileInput');
            if (!fileInput.files.length) return App.utils.toast('请先挂载待上传的文件', 'warning');
            const file = fileInput.files[0];
            const identifier = btoa(encodeURIComponent(file.name)) + `_${file.size}_${file.lastModified}`;
            const CHUNK_SIZE = 5 * 1024 * 1024;
            const totalChunks = Math.ceil(file.size / CHUNK_SIZE);
            const pContainer = document.getElementById('progressContainer');
            const pStatus = document.getElementById('uploadStatus');
            const pText = document.getElementById('progressText');
            const pBar = document.getElementById('progressBar');
            const updateUI = (index, total) => {
                const pct = Math.round((index / total) * 100);
                pText.innerText = `${pct}%`;
                pBar.style.width = `${pct}%`;
            };
            pContainer.classList.remove('hidden');
            pStatus.innerText = '正在验证断点续传状态...';
            updateUI(0, totalChunks);
            let uploadedChunks = [];
            try {
                const checkRes = await App.api.request(`/api/files/check?identifier=${identifier}`);
                if (checkRes.ok) {
                    const checkData = await checkRes.json();
                    uploadedChunks = checkData.uploaded_chunks || [];
                }
            } catch (err) {
                pContainer.classList.add('hidden');
                return App.utils.toast('秒传校验链路故障', 'error');
            }
            for (let i = 0; i < totalChunks; i++) {
                if (uploadedChunks.includes(i)) {
                    updateUI(i + 1, totalChunks);
                    continue;
                }
                pStatus.innerText = `正在传输第 ${i + 1}/${totalChunks} 块分片...`;
                const start = i * CHUNK_SIZE;
                const end = Math.min(file.size, start + CHUNK_SIZE);
                const chunkBlob = file.slice(start, end);
                const formData = new FormData();
                formData.append('file', chunkBlob);
                const url = `/api/files/upload_chunk?identifier=${identifier}&chunk_index=${i}&total_chunks=${totalChunks}&file_name=${encodeURIComponent(file.name)}&parent_path=${encodeURIComponent(App.state.currentPath)}`;
                try {
                    const res = await fetch(url, {
                        method: 'POST',
                        body: formData,
                        headers: App.api.getAuthHeaders()
                    });
                    if (res.status === 401) {
                        document.getElementById('login-overlay').classList.remove('hidden');
                        return;
                    }
                    if (!res.ok) throw new Error();
                    updateUI(i + 1, totalChunks);
                } catch (err) {
                    pStatus.innerText = '传输遭遇中断';
                    return App.utils.toast(`切片 ${i+1} 网络写出错误，请重试`, 'error');
                }
            }
            pStatus.innerText = '全部分片发送完毕，正在整合落盘...';
            try {
                const mergeRes = await App.api.request('/api/files/merge', {
                    method: 'POST', headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ identifier: identifier, file_name: file.name, parent_path: App.state.currentPath })
                });
                if (mergeRes.ok) {
                    App.utils.toast('大文件分片合并成功！上传完毕。', 'success');
                    fileInput.value = '';
                    this.fetchFiles();
                    setTimeout(() => pContainer.classList.add('hidden'), 2000);
                } else {
                    const errMsg = await mergeRes.text();
                    App.utils.toast(`合并失败: ${errMsg}`, 'error');
                    pStatus.innerText = '落盘终止';
                }
            } catch (e) { pStatus.innerText = '网关响应错误'; }
        },
        openCreateShare(fullItemPath, isDir) {
            document.getElementById('share-path').value = fullItemPath;
            document.getElementById('share-is-dir').value = isDir ? '1' : '0';
            document.getElementById('share-expire-hours').value = '';
            document.getElementById('share-password').value = '';
            document.getElementById('create-share-modal').classList.remove('hidden');
            document.getElementById('create-share-modal').style.display = 'flex';
        },
        closeCreateShareModal() {
            document.getElementById('create-share-modal').style.display = 'none';
            document.getElementById('create-share-modal').classList.add('hidden');
        },
        async confirmCreateShare() {
            const filePath = document.getElementById('share-path').value;
            const isDir = document.getElementById('share-is-dir').value === '1';
            const expireHours = document.getElementById('share-expire-hours').value;
            const password = document.getElementById('share-password').value;
            const payload = {
                file_path: filePath,
                is_dir: isDir,
                expire_hours: expireHours ? parseInt(expireHours) : null,
                password: password || null
            };
            try {
                const res = await App.api.request('/api/share/create', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify(payload)
                });
                if (res.ok) {
                    const data = await res.json();
                    App.utils.toast(`分享链接已创建: ${data.code}`, 'success');
                    this.closeCreateShareModal();
                    const shareModal = document.getElementById('share-modal');
                    if (shareModal.style.display === 'flex') {
                        this.loadShares();
                    }
                } else {
                    const err = await res.text();
                    App.utils.toast(`创建分享失败: ${err}`, 'error');
                }
            } catch(e) {
                App.utils.toast('网络错误', 'error');
            }
        },
        openShareManager() {
            if (!localStorage.getItem('cloud_auth_token')) {
                App.utils.toast('请先登录', 'warning');
                return;
            }
            document.getElementById('share-modal').classList.remove('hidden');
            document.getElementById('share-modal').style.display = 'flex';
            this.loadShares();
        },
        closeShareModal() {
            document.getElementById('share-modal').style.display = 'none';
            document.getElementById('share-modal').classList.add('hidden');
        },
        async loadShares() {
            try {
                const res = await App.api.request('/api/share/list');
                if (res.ok) {
                    App.state.shares = await res.json();
                    App.view.renderSharesList();
                } else {
                    App.utils.toast('加载分享列表失败', 'error');
                }
            } catch(e) {
                App.utils.toast('网络错误', 'error');
            }
        },
        async deleteShare(code) {
            if (!confirm('确定要删除这个分享链接吗？')) return;
            try {
                const res = await App.api.request('/api/share/delete', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ code })
                });
                if (res.ok) {
                    App.utils.toast('分享链接已删除', 'success');
                    this.loadShares();
                } else {
                    App.utils.toast('删除失败', 'error');
                }
            } catch(e) {
                App.utils.toast('网络错误', 'error');
            }
        },
        async clearTrash() {
            if (!confirm("⚠️ 确定要永久清空回收站中的所有文件吗？此操作不可恢复！")) return;
            try {
                const res = await App.api.request('/api/trash/clear', { method: 'POST' });
                if (res.ok) {
                    App.utils.toast('回收站已清空', 'success');
                    if (App.state.currentView === 'trash') {
                        this.loadTrashList();
                    }
                    if (App.state.currentView === 'drive') {
                        this.fetchQuota();
                    }
                } else {
                    const err = await res.text();
                    App.utils.toast(`清空失败: ${err}`, 'error');
                }
            } catch(e) {
                App.utils.toast('网络错误', 'error');
            }
        },
        async loadTrashList() {
            const tbody = document.getElementById('trash-list-body');
            tbody.innerHTML = '<tr><td colspan="3" class="p-8 text-center text-gray-400">加载中...</td></tr>';
            try {
                const res = await App.api.request('/api/trash/list');
                if (res.ok) {
                    const items = await res.json();
                    if (items.length === 0) {
                        tbody.innerHTML = '<tr><td colspan="3" class="p-8 text-center text-gray-400">回收站为空</td></tr>';
                        return;
                    }
                    let html = '';
                    for (const item of items) {
                        const deletedAt = new Date(item.deleted_at).toLocaleString();
                        html += `
                            <tr class="hover:bg-gray-50/80 transition">
                                <td class="p-4 font-mono text-sm">${App.utils.escapeHtml(item.original_path)}</td>
                                <td class="p-4 text-xs text-gray-500">${deletedAt}</td>
                                <td class="p-4 text-right space-x-2">
                                    <button onclick="App.actions.restoreTrashItem(${item.id})" class="text-xs font-medium text-green-600 hover:text-green-900 transition">↩️ 还原</button>
                                    <button onclick="App.actions.deleteTrashItemPermanent(${item.id})" class="text-xs font-medium text-red-600 hover:text-red-900 transition">❌ 永久删除</button>
                                </td>
                            </tr>
                        `;
                    }
                    tbody.innerHTML = html;
                } else {
                    tbody.innerHTML = '<tr><td colspan="3" class="p-8 text-center text-red-400">加载失败</td></tr>';
                }
            } catch(e) {
                tbody.innerHTML = '<tr><td colspan="3" class="p-8 text-center text-red-400">网络错误</td></tr>';
            }
        },
        async restoreTrashItem(id) {
            if (!confirm('确定要还原此文件/目录吗？')) return;
            try {
                const res = await App.api.request('/api/trash/restore', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ id })
                });
                if (res.ok) {
                    App.utils.toast('还原成功', 'success');
                    this.loadTrashList();
                    if (App.state.currentView === 'drive') {
                        this.fetchFiles();
                        this.fetchQuota();
                    }
                } else {
                    const err = await res.text();
                    App.utils.toast(`还原失败: ${err}`, 'error');
                }
            } catch(e) {
                App.utils.toast('网络错误', 'error');
            }
        },
        async deleteTrashItemPermanent(id) {
            if (!confirm('⚠️ 永久删除后无法恢复，确定要删除吗？')) return;
            try {
                const res = await App.api.request('/api/trash/delete', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ id })
                });
                if (res.ok) {
                    App.utils.toast('已永久删除', 'success');
                    this.loadTrashList();
                    if (App.state.currentView === 'drive') {
                        this.fetchQuota();
                    }
                } else {
                    const err = await res.text();
                    App.utils.toast(`删除失败: ${err}`, 'error');
                }
            } catch(e) {
                App.utils.toast('网络错误', 'error');
            }
        },
        copyShareUrl(url) {
            const copyToClipboard = (text) => {
                if (navigator.clipboard && navigator.clipboard.writeText) {
                    return navigator.clipboard.writeText(text).then(() => true).catch(() => false);
                }
                const textarea = document.createElement('textarea');
                textarea.value = text;
                textarea.style.position = 'fixed';
                textarea.style.top = '-9999px';
                textarea.style.left = '-9999px';
                document.body.appendChild(textarea);
                textarea.select();
                try {
                    const success = document.execCommand('copy');
                    document.body.removeChild(textarea);
                    return Promise.resolve(success);
                } catch (err) {
                    document.body.removeChild(textarea);
                    return Promise.resolve(false);
                }
            };
            copyToClipboard(url).then((success) => {
                if (success) {
                    App.utils.toast('分享链接已复制到剪贴板', 'success');
                } else {
                    App.utils.toast('复制失败，请手动复制', 'error');
                }
            });
        },
        async loadUserList() {
            const tbody = document.getElementById('user-list-body');
            tbody.innerHTML = '<tr><td colspan="4" class="p-8 text-center text-gray-400">加载中...</td></tr>';
            try {
                const res = await App.api.request('/api/admin/users');
                if (res.ok) {
                    const users = await res.json();
                    if (users.length === 0) {
                        tbody.innerHTML = '<tr><td colspan="4" class="p-8 text-center text-gray-400">暂无用户</td></tr>';
                        return;
                    }
                    let html = '';
                    for (const user of users) {
                        const percent = (user.used_mb / user.quota_mb) * 100;
                        html += `
                            <tr class="hover:bg-gray-50/80 transition">
                                <td class="p-4 font-mono text-sm">${App.utils.escapeHtml(user.username)}</td>
                                <td class="p-4 text-xs">${user.role === 'admin' ? '管理员' : '普通用户'}</td>
                                <td class="p-4 text-xs">
                                    ${user.used_mb} / ${user.quota_mb}
                                    <div class="w-24 bg-gray-200 rounded-full h-1.5 mt-1">
                                        <div class="bg-indigo-600 h-full rounded-full" style="width: ${Math.min(percent, 100)}%"></div>
                                    </div>
                                </td>
                                <td class="p-4 text-right space-x-2">
                                    <button onclick="App.actions.openSetQuotaModal('${user.username}', ${user.quota_mb})" class="text-xs font-medium text-blue-600 hover:text-blue-900 transition">💾 修改配额</button>
                                    <button onclick="App.actions.resetUserPassword('${user.username}')" class="text-xs font-medium text-amber-600 hover:text-amber-900 transition">🔑 重置密码</button>
                                </td>
                            </tr>
                        `;
                    }
                    tbody.innerHTML = html;
                } else {
                    tbody.innerHTML = '<tr><td colspan="4" class="p-8 text-center text-red-400">加载失败</td></tr>';
                }
            } catch(e) {
                tbody.innerHTML = '<tr><td colspan="4" class="p-8 text-center text-red-400">网络错误</td></tr>';
            }
        },
        openSetQuotaModal(username, currentQuota) {
            const newQuota = prompt(`请输入用户 ${username} 的新配额（MB）`, currentQuota);
            if (newQuota === null) return;
            const quotaMb = parseInt(newQuota, 10);
            if (isNaN(quotaMb) || quotaMb <= 0) {
                App.utils.toast('配额必须是正整数', 'error');
                return;
            }
            this.setUserQuota(username, quotaMb);
        },
        async setUserQuota(username, quotaMb) {
            try {
                const res = await App.api.request('/api/admin/quota', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ username, quota_mb: quotaMb })
                });
                if (res.ok) {
                    App.utils.toast(`用户 ${username} 配额已更新`, 'success');
                    this.loadUserList();
                } else {
                    const err = await res.text();
                    App.utils.toast(`更新失败: ${err}`, 'error');
                }
            } catch(e) {
                App.utils.toast('网络错误', 'error');
            }
        },
        async resetUserPassword(username) {
            let newPassword = prompt(`请输入 ${username} 的新密码（留空将随机生成）`, '');
            let finalPassword = newPassword;
            if (!newPassword || newPassword.trim() === '') {
                finalPassword = Math.random().toString(36).slice(-8);
                App.utils.toast(`将设置随机密码: ${finalPassword}`, 'info');
            }
            if (!confirm(`确定重置用户 ${username} 的密码为: ${finalPassword} 吗？`)) return;
            try {
                const res = await App.api.request('/api/admin/user/reset_password', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ username, new_password: finalPassword })
                });
                if (res.ok) {
                    App.utils.toast(`密码重置成功，新密码: ${finalPassword}`, 'success');
                } else {
                    const err = await res.text();
                    App.utils.toast(`重置失败: ${err}`, 'error');
                }
            } catch(e) {
                App.utils.toast('网络错误', 'error');
            }
        },
        startSystemStatusPolling() {
            this.stopSystemStatusPolling();
            this.fetchSystemStatus();
            App.state.systemStatusInterval = setInterval(() => {
                this.fetchSystemStatus();
            }, 3000);
        },
        stopSystemStatusPolling() {
            if (App.state.systemStatusInterval) {
                clearInterval(App.state.systemStatusInterval);
                App.state.systemStatusInterval = null;
            }
        },
        fetchSystemStatus: async function() {
            const token = localStorage.getItem('cloud_auth_token');
            const role = localStorage.getItem('cloud_role');
            if (!token || role !== 'admin') return;
            try {
                const res = await App.api.request('/api/system/status');
                if (res && res.ok) {
                    const data = await res.json();
                    const card = document.getElementById('system-monitor-card');
                    if (card) card.classList.remove('hidden');
                    document.getElementById('sys-cpu-text').innerText = `${data.cpu_usage.toFixed(1)}%`;
                    document.getElementById('sys-cpu-bar').style.width = `${data.cpu_usage}%`;
                    document.getElementById('sys-mem-text').innerText = `${data.memory_used_mb} / ${data.memory_total_mb} MB`;
                    const memPct = data.memory_total_mb > 0 ? (data.memory_used_mb / data.memory_total_mb) * 100 : 0;
                    document.getElementById('sys-mem-bar').style.width = `${memPct}%`;
                    const tempEl = document.getElementById('sys-temp-text');
                    tempEl.innerText = `${data.cpu_temp.toFixed(1)} °C`;
                    if (data.cpu_temp > 70) tempEl.className = "font-mono font-bold text-sm px-2 py-0.5 rounded-md bg-red-50 text-red-600 animate-pulse";
                    else if (data.cpu_temp > 55) tempEl.className = "font-mono font-bold text-sm px-2 py-0.5 rounded-md bg-amber-50 text-amber-600";
                    else tempEl.className = "font-mono font-bold text-sm px-2 py-0.5 rounded-md bg-green-50 text-green-600";
                }
            } catch (e) {
                console.error("无法拉取树莓派系统状态计数器:", e);
                // 如果是授权错误（401），停止轮询并清除本地会话
                if (e.message === 'Unauthorized') {
                    console.log("Token 已失效，停止系统状态轮询并清除登录状态");
                    App.actions.stopSystemStatusPolling();
                    localStorage.removeItem('cloud_auth_token');
                    localStorage.removeItem('cloud_username');
                    localStorage.removeItem('cloud_role');
                    App.state.username = "";
                    App.state.role = "";
                    document.getElementById('login-overlay').classList.remove('hidden');
                }
            }
        }
    }
};

// 初始化页面
window.onpopstate = function(event) {
    App.state.currentPath = event.state ? event.state.path : "";
    App.state.selectedFileNames.clear();
    App.actions.fetchFiles();
};

window.onload = () => {
    const token = localStorage.getItem('cloud_auth_token');
    if (token) {
        document.getElementById('login-overlay').classList.add('hidden');
        App.actions.updateLayoutByRole();
        const urlParams = new URLSearchParams(window.location.search);
        const targetView = urlParams.get('path') ? 'drive' : 'home';
        App.actions.switchView(targetView);
        App.actions.startSystemStatusPolling();
        App.actions.fetchLinks();
    } else {
        document.getElementById('login-overlay').classList.remove('hidden');
    }
};