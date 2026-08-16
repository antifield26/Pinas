# Antifield Cloud (Pi-NAS)

自托管 NAS 网盘应用，面向 Raspberry Pi 5 (8GB RAM, Debian 13, ARM64 cortex-a76)。

## 技术栈

| 层 | 技术 |
|---|------|
| 后端 | Rust 2024, Axum 0.8, Tokio, SQLx 0.9 (SQLite WAL), tower-http |
| 模板 | Askama 0.16 (编译时 Jinja2 语法) |
| 前端 | HTMX 2.0.4 + Alpine.js 3.14.9 |
| CSS | Tailwind CSS v4 (预编译 `tailwind.min.css`) |
| Markdown | marked.js + DOMPurify (.md 文件预览渲染) |
| 数据库 | SQLite WAL 模式 (`cloud_disk.db`) |
| 目标平台 | `aarch64-unknown-linux-gnu`, `cortex-a76` |
| 部署 | systemd 直跑二进制（Dockerfile 仅作 CI 冒烟构建） |

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
│   ├── router.rs            # build_router() — ~105 条路由
│   ├── fsutil.rs            # openat2 内核级路径沙箱（Sandbox：BENEATH + *at 族）
│   ├── templates.rs         # AppTemplate<T> — Askama → IntoResponse
│   ├── db/
│   │   ├── mod.rs           # create_pool(), init_tables()
│   │   ├── migrations.rs    # 版本化迁移 (schema_version 表, 当前 v12)
│   │   └── queries.rs       # 共享查询辅助
│   ├── middleware/
│   │   ├── csp.rs           # Content-Security-Policy
│   │   └── request_id.rs    # X-Request-Id（响应头 + tracing span 贯穿）
│   ├── tasks/
│   │   └── cleanup.rs       # 后台任务 (支持 CancellationToken)
│   └── handlers/
│       ├── pages.rs         # 页面路由 (page_handler! 宏)
│       ├── auth.rs          # 认证 (Argon2 + Secure Cookie)
│       ├── file_ops/        # 文件 CRUD（P1-1 拆分：core 核心 / api JSON / fragments HTMX）
│       ├── upload.rs        # 分片上传 (10MB/片 + 并发3 + 重试3)
│       ├── media.rs         # 媒体代理 (流式播放 + Range)
│       ├── share.rs         # 分享管理 + 分享页面
│       ├── trash.rs         # 回收站
│       ├── admin.rs         # 用户管理
│       ├── system.rs        # 健康检查 + 系统监控
│       ├── links.rs         # 链接收藏 CRUD
│       ├── todos.rs         # 待办/日程 CRUD
│       ├── minecraft.rs     # MC 服务器状态
│       ├── dav/             # WebDAV（P1-1 拆分：mod 入口 / auth 认证 / ops 方法）
│       ├── rate_limit.rs    # 异步速率限制器
│       └── utils.rs         # 配额/MIME/审计/字符串路径校验（纵深防御层）
├── core/
│   ├── mod.rs               # UserSession + 密码学重导出
│   ├── auth.rs              # auth_middleware（空闲超时滑动刷新）
│   └── crypto.rs            # hash_token/password/verify/generate
├── templates/
│   ├── base.html            # 根布局 (nav/toast/modal/PWA/JS namespace)
│   ├── pages/               # 9 页面模板 (6 个 page_struct! + 3 独立页)
│   ├── components/          # 可复用 HTMX 片段 (18 个，含 upload_queue.html)
│   └── partials/            # 片段 include (theme_head.html 独立页暗色)
├── assets/                  # 静态资源 (CSS/JS/manifest)
├── static/sw.js             # PWA Service Worker v16
├── deny.toml                # cargo-deny 供应链策略（P1-3）
└── uploads/                 # 运行时文件存储
```

## 路由

### 页面路由 (完整 HTML)

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/` | 仪表盘 |
| `GET` | `/drive` | 文件浏览器 |
| `GET` | `/todos` | 待办/日程 |
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
| `*` | `/dav`, `/dav/`, `/dav/{*path}` | WebDAV（Basic 认证：PROPFIND/GET/PUT/MKCOL/MOVE/COPY/DELETE/LOCK） |
| `GET` | `/api/system/status` | 系统状态 (admin) |
| `GET` | `/api/minecraft/status` | MC 状态 |
| `GET` | `/health` | 健康检查 |

## 数据库

12 表 + 1 虚拟表：`users`, `sessions`, `files`, `upload_chunks`, `shares`, `trash`, `audit_logs`, `links`, `todos`, `media_tokens`, `fs_journal`（文件操作意图日志，启动重放）, `schema_version` + FTS5 `files_fts`（trigram case_sensitive 0，触发器同步）；v1.11 起内置 AI Chat 相关表（user_settings/conversations/conversation_messages）已随迁移 v12 删除

- **WAL 模式** (`Normal` synchronous)，连接池 16
- **WAL checkpoint** 定时任务（每小时 `PRAGMA wal_checkpoint(TRUNCATE)`）
- **15 个显式索引**
- **版本化迁移**：`schema_version` 表 + `PRAGMA table_info` 幂等检查

## 环境变量

```
PINAS_SERVER_HOST=0.0.0.0          PINAS_SERVER_PORT=3000
PINAS_DATABASE_URL=sqlite:cloud_disk.db
PINAS_UPLOAD_LIMIT_MB=100          PINAS_DEFAULT_QUOTA_MB=10240
PINAS_SESSION_DAYS=7               PINAS_SESSION_IDLE_MINUTES=1440   # 空闲超时（默认 24h）
PINAS_DATA_DIR=
PINAS_TEMP_CLEANUP_HOURS=24        PINAS_TRASH_CLEANUP_DAYS=30
PINAS_ADMIN_PASSWORD=              PINAS_GUEST_PASSWORD=
PINAS_ALLOW_REGISTRATION=false
PINAS_COOKIE_SECURE=               PINAS_SYNC_PASSWORDS=false
MINECRAFT_HOST=127.0.0.1           MINECRAFT_PORT=25565
```

## 代码约定

### 安全约定
- **`validate_name`**：文件/文件夹名白名单（拒绝 `/` `\` `..` 引号 尖括号 控制字符），挂于建文件夹/重命名/merge 写入路径
- **`safe_join_sandbox` 返回 `AppResult`**：字符串级纵深防御（拒绝 .. / 绝对路径/盘符），保留在 18 个调用点
- **`fsutil::Sandbox`（v1.10 P0-4）**：全部物理文件操作的内核级沙箱——
  `openat2(RESOLVE_BENEATH | NO_MAGICLINKS)` + `renameat/unlinkat/mkdirat/statat` 族，
  符号链接越界（含绝对路径链接）由内核在单次系统调用内原子拒绝，TOCTOU 窗口归零；
  沙箱内相对符号链接正常可用；删除/重命名只作用于链接本身（不跟随最终组件）
- **强制下载类型** `is_force_download_mime`：html/svg/xml/js 一律 octet-stream + `Content-Disposition: attachment`（分享/媒体）
- **模板内联 JS 零用户数据**：导航走 `data-nav-path`/`data-breadcrumb-path` 事件委托；`hx-vals` 一律 `|json` 过滤器
- **限速可信源** `MaybePeer`：直连用真实对端 IP，回环(cloudflared)信任 CF-Connecting-IP，防伪造 XFF 绕过
- **注册开关** `PINAS_ALLOW_REGISTRATION`（默认 false）；分片临时存储 5GB/用户上限；备份保留 7 份轮转
- **WebDAV** `/dav/*`：Basic 认证（60s 缓存键含凭证指纹 sha256(user\0pass)，改密/重置即失效）
  + 认证限速（10 次/60s/IP）+ 全路径 openat2 沙箱 + DELETE 进回收站；路由级 5GiB body limit
- 密码学函数从 `src/core` 导入（`hash_password`, `verify_password`, `hash_token`）；
  Argon2id m=19MiB/t=3/p=1（验证参数随哈希串自描述，旧哈希不受影响）
- 分享匿名端点限速 + 每分享失败锁定
- **会话双闸（v1.10 P0-2）**：绝对过期（7 天）+ 空闲超时（默认 24h，`PINAS_SESSION_IDLE_MINUTES`）；
  中间件惰性刷新 last_active_at（≥5 分钟才写库），超时会话强制下线并定期清理
- **X-Request-Id（v1.10 P1-2）**：所有响应携带请求 ID（沿用入站值），tracing span 注入
  request_id 字段；dsh 反代读取扩展注入上游请求头
- **Cookie 默认强制 Secure**（纯 HTTP 局域网需 PINAS_COOKIE_SECURE=false）

### 错误处理
- `AppError` 枚举（11 种 HTTP 状态） + `AppResult<T>` = `Result<T, AppError>`
- JSON API → `AppResult<Json<T>>`；HTMX 片段 → `impl IntoResponse` + `AppTemplate<T>`
- 文件操作共享核心逻辑：`rename_core()` / `move_core()` / `delete_to_trash()`

### Handler 模式
- **页面** → `page_handler!` 宏（`pages.rs`）
- **片段** → Askama 模板结构体 + async fn → `AppTemplate<T>.into_response()`
- **JSON API** → `AppResult<Json<T>>` + `?` 传播
- **错误恢复** → `fallback_file_list()` 重新渲染文件列表 + `HX-Trigger: quotaRefresh`；
  失败变体 `fallback_file_list_with_error` 经 `HX-Trigger: {\"toastError\": \"…\"}` 弹错误 Toast

### 数据库
- 查询用 `sqlx::query` / `sqlx::query_as`，事务用 `pool.begin().await`；
  **配额原子性**：写路径统一 `check_and_adjust_quota_tx`（事务内预检+增量）；
  全量重算 `update_user_used_mb` 事务化（先空写抢锁再 SUM，防增量/全量互相覆盖漂移）
- 日程 `status` 仅存储 `pending`，由 `compute_effective_status()` 动态计算
- 密码学函数从 `pinas_core` 导入（`hash_password`, `verify_password`, `hash_token`）

### 模板
- `extends "base.html"` 页面需要 `username`, `is_admin`, `current_page` 三字段
- 组件独立，通过 HTMX 加载
- 变量 `snake_case`，循环 `{% for %} {% endfor %}`，条件用 Rust 风格 (`&&`, `!=`)

### 前端
- JS 全部外置于 `assets/app.js`（CSP script-src 无 'unsafe-inline'；仅两处主题预涂脚本内联，
  经 sha256 哈希放行）；交互统一 data-* 属性 + document 事件委托；
  `window.App = { showToast, closeModal, navigateTo, goParent, handleUploadForm }`
- **图标系统**：`partials/icons.html` 的 `icon(name, class)` 宏（简约线性 SVG，24×24/stroke1.5/
  currentColor）；文件类型图标由 Rust 侧 `file_icon_kind` 计算 `icon_kind` 字段；禁止使用 emoji
- **组件类**：btn-primary/secondary/ghost/danger(/btn-sm)、icon-btn、form-label、input-error、
  badge 四色、empty-state、card-hover、row-hover、skeleton——新 UI 一律引用组件类，不手写内联重复
- **动效**：View Transitions API 接管 hx-boost 整页导航（渐进增强，reduced-motion/不支持回退
  CSS 转场）；Toast 倒计时条/批量工具栏淡入在 app.js；交错延迟仍为模板内联
  animation-delay（未迁移 CSS 变量）
- 上传：10MB 分片 + 3 并发 + 3 次指数退避重试 + `/api/files/check` 断点续传；
  `window.UploadQueue` 队列面板（进度/取消/文件夹上传 webkitdirectory + 拖拽 webkitGetAsEntry 递归）
- 全局搜索：drive 页"全局"checkbox（path 置空）→ 后端 ≥3 字符走 FTS5 trigram、≤2 字符 LIKE 兜底；结果跨目录显示路径
- Markdown 渲染：`App.renderMarkdown` = DOMPurify.sanitize(marked.parse())；.md 预览用；
  preview 的 markdown 原文经 serde_json 编码 + `<` 转义嵌入 script（防 </script> 逃逸）
- 视频：`<video controls autoplay muted playsinline>` + Range 流式播放
- 暗色模式：`<head>` 同步脚本预处理 + Alpine `$watch` + localStorage（独立页共用 `partials/theme_head.html`）
- 云盘路径导航：唯一入口 `App.navigateTo(path)` / `App.goParent()`，路径来源 `#drive-current-path`
- PWA：SW v16 预缓存公开壳 `/login` + 全部本地资源（`/api/` 一律不缓存防敏感 JSON 滞留；
  预缓存登录壳而非 `/`，避免已登录 dashboard 的用户名被烤进 CacheStorage；
  版本串与模板 ?v= 严格一致，`scripts/check-versions.sh` 校验含嵌套路径与 manifest）；
  离线仅静态壳/登录页兜底，HTMX 片段与页面离线不可用
- 版本对齐：Cargo.toml → `/health` version；`?v=` 与 sw.js 预缓存 URL 严格一致（check-versions.sh 强制）

### 已知边界与接受的残余风险（审计记录，2026-08，v1.11 更新）
- CSP script-src 保留 'unsafe-eval'（Alpine x-data 表达式编译 + htmx hx-on 依赖）；style-src 保留
  'unsafe-inline'（动画延迟/进度条宽度等内联样式依赖）——**接受项**，移除需迁移 Alpine（收益不划算）
- 会话双闸（绝对 7 天 + 空闲 24h）已就位；分享失败锁定仍为内存态（重启清零）
- upload_limit_mb 语义 = 全局 body limit（非单文件上限），单文件实际由配额约束；quota_mb=0 表示禁止上传
- openat2 沙箱（RESOLVE_BENEATH）已就位；残余：绝对路径符号链接在沙箱内被整体拒绝（保守语义）
- 登录响应 JSON 回显会话 token（dsh-plugin-pinas Bearer 流程依赖）；页面 JS 不经手存储
- v1.11 已移除内置 AI Chat（代码/路由/数据表/保存的 Key 全部清理）；AI 能力收敛到 dsh（DeepSeek Harness 反代 + dsh-plugin-pinas），
  pinas 保留 dsh 所需的功能 API（files/todos/links/system 等）

### UI 规范（v1.5 起）
- **暗色层级**：页面底 `gray-950` → 卡片/导航/模态 `gray-900` → 输入/井面 `gray-800`；hover 恒比基底高一档；边框 `gray-700/800`
- **品牌**：indigo→violet 渐变仅用于主按钮与品牌字（`bg-clip-text`），其余克制
- **动画**（全部尊重 `prefers-reduced-motion`）：
  - 片段替换：容器 `fade-me`（swap 淡出 0.2s）+ 新内容 `animate-fade-in`（中央映射挂载）
  - 模态框：`animate-modal-in`（scale 0.96→1，0.15s）；Toast：`animate-toast-in/out`
  - 列表交错：行/卡片 `loop.index0 × 30ms`（封顶 240ms）
  - 页面导航（hx-boost）：`animate-page-leave`（浅淡 0.12s，不白屏）+ `animate-page-enter`（淡入上移 0.2s）；
    JS classList 动态挂载（普通 CSS 类，非 @utility——按需生成会遗漏）；失败/401 即恢复；前进后退同样淡入
  - 时长统一 ≤0.2s；`system_monitor_live`（1s 轮询）禁用动画

### 测试
- 77 个集成测试 + 15 个单元测试（数量以 CI 为准，本段为覆盖清单；含安全回归：穿越 merge/delete ".."/非法名称/分享下载头/备份有效性/
  子树迁移/媒体 Range；v1.6 新增 WebDAV 全链路、全局搜索、FTS 触发器、嵌套 merge、markdown 预览转义；
  v1.7 新增同名重传 409、重命名覆盖保护、中文多字节子树、回收站清扫豁免、FK 全连接、媒体令牌作用域、
  分享爆破锁定、SSE 截断、带时间日程日历、HSTS 门控、大小写搜索等；
  v1.10 新增符号链接越界（读/写路径 4 项）、DNS 私网 IP 分类、密钥加解密往返、会话空闲超时、X-Request-Id；
  v1.11 随内置 AI Chat 移除，AI 相关测试（agent/conversation/settings/api_base）同步删除）
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
- `cloud.antifield.work → http://localhost:3000`；`pidsh.antifield.work → http://localhost:3100`（dsh 反代，admin 会话门禁）；`mc.antifield.work → tcp://localhost:25565`
- **`protocol: auto`**（QUIC 优先，UDP 失败自动退化 HTTP/2）——曾因 UDP 被 QoS 触发 502 改为 http2，
  TCP 又被限速导致 TLS 5-15s；QUIC 无队头阻塞 + 0-RTT，为当前最优
- 边缘节点 LAX/SJC（回环 RTT ~200ms），应用本地 TTFB 1ms，瓶颈全在运营商链路
- 本机 nginx 为默认站点，未参与反代

**性能要点**（2026-08 实测）：
- 全部前端依赖本地化（htmx/alpine 原走 unpkg，国内链路不可控）；marked/purify 由 `App.renderMarkdown` 首次调用时动态注入（懒加载，全站不预载 70KB）
- 脚本加 `data-cfasync="false"` 防 Rocket Loader 异步化破坏 htmx 同步时序
- login/change_password 公开页带 `Cache-Control: public, max-age=60`（浏览器缓存；
  CF 边缘缓存 HTML 需面板 Cache Everything 规则）
- 静态资源 CF 自动 gzip + 缓存（max-age=86400，cf-cache-status: HIT）
