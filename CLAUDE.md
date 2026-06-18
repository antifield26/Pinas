# Antifield Cloud (Pi-NAS)

自托管 NAS 网盘应用，面向 Raspberry Pi 5 (8GB RAM, Debian13)，提供文件管理、待办日程、AI 助手、链接收藏等功能。

## 技术栈

| 层 | 技术 |
|---|------|
| 后端 | Rust 2021, Axum 0.7, Tokio, SQLx 0.7 (SQLite WAL), tower-http |
| 数据库 | SQLite (WAL 模式, `cloud_disk.db`) |
| 前端 | 原生 JavaScript ES Modules, Tailwind CSS v4, marked + DOMPurify |
| 目标平台 | `aarch64-unknown-linux-gnu` / `aarch64-unknown-linux-musl`, `cortex-a76` |
| 部署 | Dockerfile (多阶段构建), 默认端口 3000 |

## 项目结构

```
├── src/
│   ├── main.rs              # 入口点 (~56 行，加载配置/日志/DB/路由/任务)
│   ├── config.rs            # Config 结构体 (PINAS_* 环境变量)
│   ├── constants.rs         # 全局常量 (路径/角色/限制/间隔)
│   ├── error.rs             # AppError 统一错误类型 + AppResult<T>
│   ├── router.rs            # build_router() — 所有路由注册
│   ├── db/
│   │   ├── mod.rs           # create_pool(), init_tables(), init_indexes(), init_default_users()
│   │   └── migrations.rs    # ALTER TABLE 兼容性迁移
│   ├── middleware/
│   │   ├── mod.rs
│   │   └── csp.rs           # Content-Security-Policy 中间件
│   ├── tasks/
│   │   ├── mod.rs
│   │   └── cleanup.rs       # 后台清理任务 (临时分片/日志/回收站/速率限制)
│   └── handlers/
│       ├── mod.rs           # 模块声明 + BatchDownloadRequest DTO
│       ├── auth.rs          # login, register, logout (Argon2 + Token)
│       ├── utils.rs         # hash_password, generate_random_password, safe_join_sandbox, MIME 检查
│       ├── rate_limit.rs    # 内存速率限制器 (HashMap + Mutex, 容量上限 10k)
│       ├── file_ops.rs      # list_files, create_folder, rename_item, move_item, move_batch, delete_item
│       ├── upload.rs        # check_chunk, upload_chunk, merge_chunks (分片/秒传)
│       ├── media.rs         # media_proxy (Range 支持), download_zip, editor read/save
│       ├── share.rs         # create_share, list_shares, delete_share, access_share, share_page
│       ├── trash.rs         # list_trash, restore_trash, delete_trash_permanent, clear_trash
│       ├── admin.rs         # get_user_quota, set_user_quota, list_users, reset_user_password, audit_logs
│       ├── system.rs        # health_check (DB SELECT 1), get_system_status (CPU/内存/温度)
│       ├── links.rs         # CRUD 链接收藏 (AppResult 模式)
│       ├── todos.rs         # CRUD 待办/日程 (自动状态计算: pending/in_progress/expired)
│       ├── agent.rs         # AI 对话代理 (DeepSeek API), generate_briefing, get_models
│       └── settings.rs      # GET/PUT AI Agent 用户设置 (API key/base/model)
├── pinas-core/
│   └── src/
│       ├── lib.rs           # UserSession 结构体
│       └── auth.rs          # hash_token (SHA-256) + auth_middleware
├── assets/
│   ├── css/  (input.css, tailwind.min.css)
│   ├── js/   (app.js, api.js, state.js, constants.js, utils.js, components/, views/)
│   ├── marked.min.js, purify.min.js
│   └── manifest.json        # PWA Web App Manifest
├── static/
│   ├── index.html           # SPA 入口
│   └── sw.js                # PWA Service Worker
├── uploads/                 # 运行时文件存储
├── logs/                    # 运行时日志 (每日轮转)
├── .env                     # 环境变量配置
├── Dockerfile               # ARM64 多阶段构建
└── .dockerignore
```

## 数据库模式 (10 表)

| 表 | 用途 |
|----|------|
| `users` | 用户 (Argon2 哈希, quota_mb, used_mb, must_change_pwd) |
| `sessions` | Bearer token (SHA-256 哈希, expires_at) |
| `files` | 文件索引 (username, name, parent_path, is_dir, size_mb, identifier) |
| `upload_chunks` | 分片上传跟踪 (identifier, total_chunks) |
| `shares` | 分享链接 (code, file_path, password, expires_at, download_count) |
| `trash` | 回收站 (original_path, trash_uuid, deleted_at) |
| `audit_logs` | 审计日志 (username, action, target, ip_address, user_agent) |
| `links` | 链接收藏 (title, url, icon, sort_order) |
| `todos` | 待办/日程 (due_date, is_all_day, start_time, end_time, priority, status, category) |
| `user_settings` | AI Agent 设置 (deepseek_api_key/base/model) |

## API 路由

### 公开 (无需认证)
- `POST /api/login` / `POST /api/register` / `POST /api/logout`
- `GET /api/share/access/:code`
- `GET /s/:share_id` / `GET /s/:share_id/*file_path`
- `GET /api/agent/models`
- `GET /health` (DB 连接检查 → JSON)
- `GET /sw.js` (PWA Service Worker)

### 受保护 (Bearer Token 认证)
- `/api/files/*` — 文件 CRUD + 分片上传/合并/打包下载
- `/api/edit/*` — 文本编辑器读写
- `/api/media/*path` — 多媒体流代理 (支持 Range)
- `/api/share/*` — 分享管理
- `/api/trash/*` — 回收站
- `/api/admin/*` — 用户管理/配额/密码重置/审计日志
- `/api/links` / `/api/links/:id` — 链接 CRUD
- `/api/todos` / `/api/todos/:id` — 待办/日程 CRUD
- `/api/agent/chat` / `/api/agent/briefing` / `/api/agent/settings` — AI Agent
- `/api/system/status` — 系统监控 (仅管理员)

## 关键设计决策

### 安全
- **密码**: Argon2 哈希, 首次启动无环境变量时自动生成 24 位随机密码 + `must_change_pwd` 强制修改
- **路径穿越**: `safe_join_sandbox` 拒绝 `..` / 绝对路径 / Windows 盘符, 跨平台 `ParentDir`/`CurDir` 处理
- **上传**: MIME 首 512 字节 + 完整文件两阶段安全检测, identifier 白名单校验 (字母数字+连字符)
- **速率限制**: 登录 10/min, 注册 3/hour, 容量上限 10k 条目 + LRU 淘汰
- **CSP 头**: 严格 `default-src 'self'`, 禁止 `object-src`

### 性能
- SQLite WAL 模式 (`Normal` synchronous), 连接池 16, busy_timeout 10s
- 16 个索引覆盖高频查询 (username, parent_path, identifier, code, expires_at 等)
- 大文件分块上传 (100 MB/块, 最多 10,000 块, 秒传去重)
- ZIP 打包下载 `spawn_blocking` 不阻塞异步运行时
- 后台任务: 临时分片清理/日志轮转(7天)/回收站过期(30天)/速率过期清理

### PWA
- 可安装到主屏幕 (standalone 模式), iOS/Android 适配
- Service Worker: Cache First (静态资源) / Network First (API)
- 离线降级: API 返回 `{"error":"..."}` JSON, HTML 回退到缓存
- beforeinstallprompt 自定义安装按钮 + updatefound 更新通知

## 环境变量 (`PINAS_*` 前缀)

```
PINAS_SERVER_HOST=0.0.0.0
PINAS_SERVER_PORT=3000
PINAS_DATABASE_URL=sqlite:cloud_disk.db
PINAS_UPLOAD_LIMIT_MB=10240
PINAS_DEFAULT_QUOTA_MB=10240
PINAS_SESSION_DAYS=7
PINAS_TEMP_CLEANUP_HOURS=24
PINAS_TRASH_CLEANUP_DAYS=30
PINAS_TRASH_CLEANUP_INTERVAL_HOURS=24
PINAS_ADMIN_PASSWORD=           # 留空则自动生成随机密码
PINAS_GUEST_PASSWORD=           # 留空则自动生成随机密码
PINAS_DEEPSEEK_API_KEY=         # AI Agent (可选)
PINAS_DEEPSEEK_API_BASE=https://api.deepseek.com
PINAS_DEEPSEEK_MODEL=deepseek-v4-flash
PINAS_DATA_DIR=                 # 工作目录 (可选)
```

## 构建与运行

```bash
# 开发
cargo run

# 生产构建 (ARM64, LTO+strip)
cargo build --release --target aarch64-unknown-linux-gnu

# Docker
docker build -t antifield-cloud .
docker run -p 3000:3000 -v ./data:/app/data -v ./uploads:/app/uploads antifield-cloud

# CSS 编译
npm run css:build
```

## 代码约定

- **错误处理**: `AppError` 枚举 (10 种 HTTP 状态码映射) + `AppResult<T>` 类型别名, 支持 `?` 传播
- **路由 Handler 返回**: `links.rs` 使用 `AppResult<T>` 模式, 其他 handler 使用 `impl IntoResponse`
- **数据库**: 查询用 `sqlx::query` (不用 ORM), 事务用 `pool.begin().await`, 兼容性迁移用 `ALTER TABLE ... IF NOT EXISTS` 模式 (忽略重复列错误)
- **路径**: 所有文件系统路径通过 `safe_join_sandbox` 校验, 目录常量来自 `constants.rs`
- **日志**: `tracing` crate, 双输出 (控制台+文件), `#[tracing::instrument]` 用于关键 handler
- **前端**: ES Modules 无打包, Tailwind 预编译为 `tailwind.min.css`, 全局事件委托 + 懒加载视图
