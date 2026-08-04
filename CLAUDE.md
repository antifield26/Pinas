# Antifield Cloud (Pi-NAS)

自托管 NAS 网盘应用，面向 Raspberry Pi 5 (8GB RAM, Debian 13, ARM64 cortex-a76)。

## 技术栈

| 层 | 技术 |
|---|------|
| 后端 | Rust 2024, Axum 0.8, Tokio, SQLx 0.9 (SQLite WAL), tower-http |
| 模板 | Askama 0.16 (编译时 Jinja2 语法) |
| 前端 | HTMX 2.0.4 + Alpine.js 3.14.9 |
| CSS | Tailwind CSS v4 (预编译 `tailwind.min.css`) |
| Markdown | marked.js + DOMPurify (AI 聊天渲染) |
| 数据库 | SQLite WAL 模式 (`cloud_disk.db`) |
| 目标平台 | `aarch64-unknown-linux-gnu`, `cortex-a76` |
| 部署 | Docker (多阶段构建) + systemd |

## 架构

```
Browser                      Axum Server
┌──────────────┐  HTML       ┌─────────────────────┐
│ HTMX         │◄──────────→│ Askama Templates     │
│ Alpine.js    │  fragments  │  ├ base.html         │
│ Tailwind     │             │  ├ pages/*.html (9)  │
└──────────────┘  JSON       │  └ components/*.html │
                  ◄──────────→│                     │
                              │ JSON API            │
                              │  ├ /api/files/*     │
                              │  ├ /api/todos/*     │
                              │  ├ /api/agent/*     │
                              │  └ ...              │
                              └─────────────────────┘
```

### 核心原则

- **HATEOAS**：响应包含自身的链接/操作，无客户端路由
- **服务端唯一真相源**：Alpine.js 仅用于 UI 临时状态（主题、模态框）
- **文件即真相**：文件列表以磁盘实际文件为准，DB 记录自动同步清理
- **渐进增强**：核心流程无需 JS，HTMX 增强交互

## 项目结构

```
├── src/
│   ├── main.rs              # 入口 (配置/日志/DB/路由/CancellationToken/优雅关闭)
│   ├── config.rs            # Config 结构体 (PINAS_* 环境变量 + validate())
│   ├── constants.rs         # 全局常量
│   ├── error.rs             # AppError + AppResult<T>
│   ├── router.rs            # build_router() — ~95 条路由
│   ├── templates.rs         # AppTemplate<T> — Askama → IntoResponse
│   ├── db/
│   │   ├── mod.rs           # create_pool(), init_tables()
│   │   ├── migrations.rs    # 版本化迁移 (schema_version 表)
│   │   └── queries.rs       # 共享查询辅助
│   ├── middleware/
│   │   └── csp.rs           # Content-Security-Policy
│   ├── tasks/
│   │   └── cleanup.rs       # 后台任务 (支持 CancellationToken)
│   └── handlers/
│       ├── pages.rs         # 页面路由 (page_handler! 宏)
│       ├── auth.rs          # 认证 (Argon2 + Secure Cookie)
│       ├── file_ops.rs      # 文件 CRUD + HTMX 片段 (~630行)
│       ├── upload.rs        # 分片上传 (10MB/片 + 并发3 + 重试3)
│       ├── media.rs         # 媒体代理 (流式播放 + Range)
│       ├── share.rs         # 分享管理 + 分享页面
│       ├── trash.rs         # 回收站
│       ├── admin.rs         # 用户管理
│       ├── system.rs        # 健康检查 + 系统监控
│       ├── links.rs         # 链接收藏 CRUD
│       ├── todos.rs         # 待办/日程 CRUD
│       ├── agent.rs         # AI 对话 (DeepSeek)
│       ├── conversations.rs # 对话管理 (CRUD + HTMX 片段)
│       ├── settings.rs      # Agent 用户设置
│       ├── minecraft.rs     # MC 服务器状态
│       ├── dav.rs           # WebDAV 端点 (PROPFIND/PUT/MOVE/COPY 等, Basic 认证)
│       ├── rate_limit.rs    # 异步速率限制器
│       └── utils.rs         # 路径沙箱/MIME/审计/配额
├── core/
│   ├── mod.rs               # UserSession + 密码学重导出
│   ├── auth.rs              # auth_middleware
│   └── crypto.rs            # hash_token/password/verify/generate
├── templates/
│   ├── base.html            # 根布局 (nav/toast/modal/PWA/JS namespace)
│   ├── pages/               # 10 页面模板 (7 个 page_struct! + 3 独立页)
│   ├── components/          # 可复用 HTMX 片段 (22 个，含 upload_queue.html)
│   └── partials/            # 片段 include (theme_head.html 独立页暗色)
├── assets/                  # 静态资源 (CSS/JS/manifest)
├── static/sw.js             # PWA Service Worker v11
└── uploads/                 # 运行时文件存储
```

## 路由

### 页面路由 (完整 HTML)

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/` | 仪表盘 |
| `GET` | `/drive` | 文件浏览器 |
| `GET` | `/todos` | 待办/日程 |
| `GET` | `/agent` | AI 助手 |
| `GET` | `/links` | 链接收藏 |
| `GET` | `/trash` | 回收站 |
| `GET` | `/admin` | 用户管理 |
| `GET` | `/login` | 登录 (公开) |
| `GET` | `/s/{share_id}` | 分享页面 (公开) |

### 片段路由 (HTMX HTML 局部)

| 方法 | 路径 | 用途 |
|------|------|------|
| `GET` | `/drive/list?path=&search=` | 文件列表 |
| `GET` | `/drive/breadcrumbs?path=` | 面包屑 |
| `GET` | `/drive/quota` | 配额条 |
| `POST` | `/drive/create-folder` | 创建文件夹 |
| `POST` | `/drive/delete` | 删除文件 |
| `GET/POST` | `/drive/upload-form`, `/drive/rename-form`, `/drive/move-form` | 操作表单 |
| `POST` | `/drive/rename`, `/drive/move` | 执行操作 |
| `GET` | `/drive/preview` | 文件预览 |
| `GET` | `/todos/list`, `/todos/calendar` | 待办列表/日历 |
| `GET/POST` | `/todos/form`, `/todos` | 表单/创建 |
| `PUT/DELETE` | `/todos/:id` | 更新/删除 |
| `GET/POST` | `/links/list`, `/links` | 链接列表/创建 |
| `PUT/DELETE` | `/links/:id` | 更新/删除 |
| `POST` | `/agent/chat`, `/agent/briefing` | AI 对话/简报 |
| `GET` | `/agent/settings-form` | 设置表单 |
| `GET` | `/trash/list` | 回收站列表 |
| `POST` | `/trash/clear` | 清空回收站 |
| `GET` | `/home/system-monitor` | 系统监控 |
| `GET` | `/home/minecraft-status` | MC 状态 |

### JSON API

| 方法 | 路径 | 用途 |
|------|------|------|
| `POST` | `/api/login`, `/api/register`, `/api/logout` | 认证 |
| `GET` | `/api/files/list`, `/api/files/check` | 文件列表/分片检查 |
| `POST` | `/api/files/create_folder`, `/api/files/upload_chunk`, `/api/files/merge` | 文件操作 |
| `POST` | `/api/files/delete`, `/api/files/delete_batch`, `/api/files/rename`, `/api/files/move` | 批操作 |
| `POST` | `/api/move_batch`, `/api/files/download_zip` | 批移动/下载 |
| `GET/HEAD` | `/api/media/{*path}` | 媒体流代理 |
| `GET/POST` | `/api/edit/get`, `/api/edit/save` | 文本编辑器 |
| `GET/POST` | `/api/share/*` | 分享管理 |
| `GET/POST` | `/api/trash/*` | 回收站 |
| `GET/POST` | `/api/admin/*` | 用户管理 |
| `GET/POST/PUT/DELETE` | `/api/links`, `/api/links/:id` | 链接 CRUD |
| `GET/POST/PUT/DELETE` | `/api/todos`, `/api/todos/:id` | 待办 CRUD |
| `POST` | `/api/agent/chat`, `/api/agent/chat/stream` | AI 对话（非流式 / SSE 流式 + 工具调用） |
| `POST` | `/api/agent/briefing` | AI 简报 |
| `*` | `/dav`, `/dav/`, `/dav/{*path}` | WebDAV（Basic 认证：PROPFIND/GET/PUT/MKCOL/MOVE/COPY/DELETE/LOCK） |
| `GET/PUT` | `/api/agent/settings` | Agent 设置 |
| `GET` | `/api/system/status` | 系统状态 (admin) |
| `GET` | `/api/minecraft/status` | MC 状态 |
| `GET` | `/health` | 健康检查 |

## 数据库

13 表 + 1 虚拟表：`users`, `sessions`, `files`, `upload_chunks`, `shares`, `trash`, `audit_logs`, `links`, `todos`, `user_settings`, `conversations`, `conversation_messages`, `schema_version` + FTS5 `files_fts`（trigram，触发器同步）

- **WAL 模式** (`Normal` synchronous)，连接池 16
- **WAL checkpoint** 定时任务（每小时 `PRAGMA wal_checkpoint(TRUNCATE)`）
- **15 个显式索引**
- **版本化迁移**：`schema_version` 表 + `PRAGMA table_info` 幂等检查

## 环境变量

```
PINAS_SERVER_HOST=0.0.0.0          PINAS_SERVER_PORT=3000
PINAS_DATABASE_URL=sqlite:cloud_disk.db
PINAS_UPLOAD_LIMIT_MB=100          PINAS_DEFAULT_QUOTA_MB=10240
PINAS_SESSION_DAYS=7               PINAS_DATA_DIR=
PINAS_TEMP_CLEANUP_HOURS=24        PINAS_TRASH_CLEANUP_DAYS=30
PINAS_ADMIN_PASSWORD=              PINAS_GUEST_PASSWORD=
PINAS_DEEPSEEK_API_KEY=            PINAS_DEEPSEEK_API_BASE=https://api.deepseek.com
PINAS_DEEPSEEK_MODEL=deepseek-v4-flash     PINAS_ALLOW_REGISTRATION=false
MINECRAFT_HOST=127.0.0.1           MINECRAFT_PORT=25565
```

## 代码约定

### 安全约定
- **`validate_name`**：文件/文件夹名白名单（拒绝 `/` `\` `..` 引号 尖括号 控制字符），挂于建文件夹/重命名/merge 写入路径
- **`safe_join_sandbox` 返回 `AppResult`**：攻击检测失败返回 Err（不可回退到 base），18 个调用点 `?` 传播
- **强制下载类型** `is_force_download_mime`：html/svg/xml/js 一律 octet-stream + `Content-Disposition: attachment`（分享/媒体）
- **模板内联 JS 零用户数据**：导航走 `data-nav-path`/`data-breadcrumb-path` 事件委托；`hx-vals` 一律 `|json` 过滤器
- **限速可信源** `MaybePeer`：直连用真实对端 IP，回环(cloudflared)信任 CF-Connecting-IP，防伪造 XFF 绕过
- **注册开关** `PINAS_ALLOW_REGISTRATION`（默认 false）；分片临时存储 5GB/用户上限；备份保留 7 份轮转
- **WebDAV** `/dav/*`：Basic 认证（60s 成功缓存防每请求 argon2）+ 全路径沙箱 + DELETE 进回收站；
  路由级 5GiB body limit；dav.rs 文件操作统一 std::fs（测试环境 tokio::fs 相对路径有 ENOENT 竞态）
- 密码学函数从 `src/core` 导入（`hash_password`, `verify_password`, `hash_token`）

### 错误处理
- `AppError` 枚举（11 种 HTTP 状态） + `AppResult<T>` = `Result<T, AppError>`
- JSON API → `AppResult<Json<T>>`；HTMX 片段 → `impl IntoResponse` + `AppTemplate<T>`
- 文件操作共享核心逻辑：`rename_core()` / `move_core()` / `delete_to_trash()`

### Handler 模式
- **页面** → `page_handler!` 宏（`pages.rs`）
- **片段** → Askama 模板结构体 + async fn → `AppTemplate<T>.into_response()`
- **JSON API** → `AppResult<Json<T>>` + `?` 传播
- **错误恢复** → `fallback_file_list()` 重新渲染文件列表 + `HX-Trigger: quotaRefresh`

### 数据库
- 查询用 `sqlx::query` / `sqlx::query_as`，事务用 `pool.begin().await`
- 日程 `status` 仅存储 `pending`，由 `compute_effective_status()` 动态计算
- 密码学函数从 `pinas_core` 导入（`hash_password`, `verify_password`, `hash_token`）

### 模板
- `extends "base.html"` 页面需要 `username`, `is_admin`, `current_page` 三字段
- 组件独立，通过 HTMX 加载
- 变量 `snake_case`，循环 `{% for %} {% endfor %}`，条件用 Rust 风格 (`&&`, `!=`)

### 前端
- JS 命名空间 `window.App = { showToast, closeModal, navigateTo, goParent, handleUploadForm }`
- 上传：10MB 分片 + 3 并发 + 3 次指数退避重试 + `/api/files/check` 断点续传；
  `window.UploadQueue` 队列面板（进度/取消/文件夹上传 webkitdirectory + 拖拽 webkitGetAsEntry 递归）
- 全局搜索：drive 页"全局"checkbox（path 置空）→ 后端 ≥3 字符走 FTS5 trigram、≤2 字符 LIKE 兜底；结果跨目录显示路径
- Markdown 渲染：`App.renderMarkdown` = DOMPurify.sanitize(marked.parse())；AI 回复 / .md 预览共用；
  preview 的 markdown 原文经 serde_json 编码 + `<` 转义嵌入 script（防 </script> 逃逸）
- AI 流式：`/api/agent/chat/stream` SSE；工具调用 5 轮循环（search_files/read_file/list_todos/create_todo/get_system_status(admin)）
- 视频：`<video controls autoplay muted playsinline>` + Range 流式播放
- 暗色模式：`<head>` 同步脚本预处理 + Alpine `$watch` + localStorage（独立页共用 `partials/theme_head.html`）
- 云盘路径导航：唯一入口 `App.navigateTo(path)` / `App.goParent()`，路径来源 `#drive-current-path`
- PWA：SW v11 预缓存全部本地资源（含 marked/purify；版本串与模板 ?v= 严格一致，`scripts/check-versions.sh` 校验），离线可用
- 版本对齐：Cargo.toml → `/health` version；`?v=` 与 sw.js 预缓存 URL 严格一致（check-versions.sh 强制）

### UI 规范（v1.5 起）
- **暗色层级**：页面底 `gray-950` → 卡片/导航/模态 `gray-900` → 输入/井面 `gray-800`；hover 恒比基底高一档；边框 `gray-700/800`
- **品牌**：indigo→violet 渐变仅用于主按钮与品牌字（`bg-clip-text`），其余克制
- **动画**（全部尊重 `prefers-reduced-motion`）：
  - 片段替换：容器 `fade-me`（swap 淡出 0.2s）+ 新内容 `animate-fade-in`（中央映射挂载）
  - 模态框：`animate-modal-in`（scale 0.96→1，0.15s）；Toast：`animate-toast-in/out`
  - 列表交错：行/卡片 `loop.index0 × 30ms`（封顶 240ms）；聊天消息 `animate-slide-up`
  - 页面导航（hx-boost）：`animate-page-leave`（浅淡 0.12s，不白屏）+ `animate-page-enter`（淡入上移 0.2s）；
    JS classList 动态挂载（普通 CSS 类，非 @utility——按需生成会遗漏）；失败/401 即恢复；前进后退同样淡入
  - 时长统一 ≤0.2s；`system_monitor_live`（1s 轮询）禁用动画

### 测试
- 33 个集成测试 + 11 个单元测试（含安全回归：穿越 merge/delete ".."/非法名称/分享下载头/备份有效性/
  子树迁移/媒体 Range；v1.6 新增 WebDAV 全链路、全局搜索、FTS 触发器、嵌套 merge、markdown 预览转义、AI 流式 503）
- 覆盖：auth 流程（含 Cookie 登出/改密 Secure）、文件 CRUD、真实分片上传/配额强制、分享密码全流程、回收站、链接/待办 CRUD、健康检查

## 构建与部署

**部署路径**: `~/pinas/`（生产环境），开发路径 `~/projects/pinas/`

```bash
# 构建
cargo build --release

# CSS 构建（改模板类名后需要；产物提交进 git，部署时连带复制）
# 首次需 npm ci 安装依赖（含 aarch64 watcher prebuild，若缺：npm install @parcel/watcher-linux-arm64-glibc）
npm run css:build          # 产物 assets/css/tailwind.min.css
# 改 CSS 后记得同步：base.html 等 4 处 ?v= 版本号 与 sw.js 缓存 key

# 部署
sudo systemctl stop antifield-cloud.service
cp target/release/pi_nas ~/pinas/pi_nas
cp static/sw.js ~/pinas/static/sw.js          # PWA 更新时
cp assets/css/tailwind.min.css ~/pinas/assets/css/  # CSS 更新时
cp assets/manifest.json ~/pinas/assets/manifest.json  # manifest 更新时
sudo systemctl start antifield-cloud.service

# 验证
curl -s http://localhost:3000/health
```

**部署目录结构**:
```
~/pinas/
├── pi_nas                  # 二进制
├── pi_nas.bak.*            # 自动备份
├── .env                    # 环境变量
├── cloud_disk.db           # SQLite 数据库
├── antifield-cloud.service # systemd unit → /etc/systemd/system/
├── uploads/                # 用户文件
├── logs/                   # 运行日志
├── backups/                # 数据库备份
├── assets/                 # 静态资源 (CSS/JS/manifest)
└── static/                 # PWA Service Worker
```

**systemd 服务**: `antifield-cloud.service`，已启用自动启动。

**Cloudflare 隧道**（`cloudflared.service`，公网入口，`/etc/cloudflared/config.yml`）：
- `drive.antifield.work → http://localhost:3000`；`mc.antifield.work → tcp://localhost:25565`
- **`protocol: auto`**（QUIC 优先，UDP 失败自动退化 HTTP/2）——曾因 UDP 被 QoS 触发 502 改为 http2，
  TCP 又被限速导致 TLS 5-15s；QUIC 无队头阻塞 + 0-RTT，为当前最优
- 边缘节点 LAX/SJC（回环 RTT ~200ms），应用本地 TTFB 1ms，瓶颈全在运营商链路
- 本机 nginx 为默认站点，未参与反代

**性能要点**（2026-08 实测）：
- 全部前端依赖本地化（htmx/alpine 原走 unpkg，国内链路不可控）；marked/purify 仅 AI 页按需加载
- 脚本加 `data-cfasync="false"` 防 Rocket Loader 异步化破坏 htmx 同步时序
- login/change_password 公开页带 `Cache-Control: public, max-age=60`（浏览器缓存；
  CF 边缘缓存 HTML 需面板 Cache Everything 规则）
- 静态资源 CF 自动 gzip + 缓存（max-age=86400，cf-cache-status: HIT）
