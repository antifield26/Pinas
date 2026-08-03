# Changelog

## v1.5.0 (2026-08-03)

### Added
- UI 重设计：暗色层级升级（950/900/800 三级）、indigo→violet 品牌渐变
- 动画体系接线（此前全部为死代码）：片段替换淡入上移、模态缩放、Toast 滑入滑出、
  列表交错入场（30ms 封顶 240ms）、聊天消息上移；全局尊重 prefers-reduced-motion
- Toast 系统修复：Alpine x-for 渲染模板 + 三色（此前 store 无渲染，toast 不可见）
- 导航 Askama 循环化：`nav_items()` 单一来源，admin 按权限过滤
- 独立页（登录/改密/分享）暗色切换按钮 + 共用 `partials/theme_head.html`
- 页面导航过渡（hx-boost）：离场浅淡 0.12s（不白屏）+ 进场淡入上移 0.2s，失败即恢复，
  前进/后退同淡入；动画类为普通 CSS 类（JS classList 动态挂载）

### Fixed
- **注册用户 quota_mb 为 NULL 被当 0 处理 → 无法上传**（register 显式写入默认配额）
- **share_page 路径解析缺 username 组件**（目录分享 is_dir 判定错误，与 share_subfile 对齐）
- logout 仅认 Authorization 头，Cookie 登录的会话无法服务端清理 → 支持 Cookie 场景
- change_password 新会话 Cookie 缺 Secure 标志（复用 X-Forwarded-Proto 检测）
- 错误消息泄露 IO/DB 细节（12 处）→ `AppError::internal_log` 仅入日志
- 云盘"↑ 上级"双机制 + HTML 实体 hack → `App.goParent()` 统一入口
- admin.html 权限文案双渲染、认证页输入框三套样式、行内按钮两套风格、
  空状态三种 padding、file_table 复选框列宽不一致
- sw.js 预缓存 key 与 base.html 版本号不匹配（marked/purify 缓存 miss）→ 对齐
- credentials.txt 写入后 chmod 600 + 删除提示
- `/health` version 返回 1.4.2（Cargo.toml 未同步）→ 1.5.0
- sw.js 预缓存 CSS 版本用 `?v=v7` 插值，与 HTML `?v=15` 不匹配（预缓存 miss）→ 统一固定版本号
- clippy 44 处既有警告清零（if-let 合并/事务回滚同步化/类型别名等）+ rustfmt 全库统一
- 部署路径 `~/pi/cloud_drive` → `~/pinas`（systemd unit 同步，开发机即部署机）

### Security
- 限速兜底：无代理直连（无 IP 头）时按 username 限速（login/register）

### Tests
- 12 个集成测试：真实 multipart 3×64KB 分片端到端（字节完整性）、配额 403、
  分享密码全流程（表单→错误→正确→删除）、logout Cookie 清理、改密 Secure 标志

### Build
- aarch64 CSS 构建修复：npm ci 恢复 @parcel/watcher prebuild；清理失效 @source
- .gitignore 补齐 backups/uploads/*.db 通配

---

## v1.4.1 (2026-07-07)

### Changed
- JS 全局函数归入 `window.App` 命名空间（`showToast`, `closeModal`, `handleUploadForm`）
- 模板中所有 `closeModal()` → `App.closeModal()`，`showToast()` → `App.showToast()`
- 待办标签高亮从 `htmx:afterRequest` 改为 click 事件委托

### Fixed
- 暗色模式页面闪白：`<head>` 同步脚本在 Alpine 加载前设置 `dark` class
- 待办/日程筛选器标签选中后高亮不切换
- 文件操作后配额占用显示不同步

### Security
- 模态框 `innerHTML` 赋值前调用 `DOMPurify.sanitize()`

### Accessibility
- 移除 `user-scalable=no`，恢复浏览器缩放
- 添加 skip-link（跳到主内容）
- 图标按钮添加 `aria-label`
- 全局 `focus-visible` 键盘焦点样式

---

## v1.4.0 (2026-07-07)

### Added
- 上传断点续传：`/api/files/check` 返回已上传分片列表，前端跳过已存在分片
- 上传秒传：文件已存在时跳过上传直接返回成功
- WAL checkpoint 定时任务（每小时 `PRAGMA wal_checkpoint(TRUNCATE)`）
- 上传分片速率限制（每用户 120 片/分钟）
- 分享页面：独立 Askama 模板，支持密码验证和文件下载
- 文件系统同步：列表查询时校验磁盘存在性，自动清理 DB 孤儿记录
- 健康检查增强：返回版本号和 DB 状态
- Agent 系统提示 30s 内存缓存
- 配置启动时校验 `Config::validate()`
- 4 个新集成测试（上传分片、分享密码、回收站、链接/待办 CRUD）

### Changed
- 上传分片大小：2MB → 10MB
- 上传并发：串行 → 3 并发
- 上传重试：无 → 3 次指数退避（1s/2s/4s）
- 速率限制器：`std::sync::Mutex` → `tokio::sync::Mutex`
- `file_ops.rs` 重构：1081→629 行，提取 `rename_core`/`move_core`/`delete_to_trash` 共享逻辑
- 路径规范化：`normalize_display_path` 折叠连续斜杠
- PWA Service Worker v6：预缓存 CDN 依赖，离线可用，不强制刷新

### Fixed
- `safe_join_sandbox` 绝对路径绕过（RootDir/Prefix 组件未提前返回）
- `share_page` 读取已删除的 `static/index.html`（404）
- 分享密码明文比对 → Argon2 `verify_password` + `spawn_blocking`
- Cookie 缺少 `Secure` 标志（基于 `X-Forwarded-Proto` 自动检测）
- `.m4v` MIME 类型浏览器不兼容（`video/x-m4v` → `video/mp4`）
- 视频 `autoplay` 缺少 `muted` 和 `playsinline`
- 视频预览无法拖动进度条：媒体代理添加 HEAD 支持 + 禁用 gzip + 初始 2MB cap
- 文件列表路径双斜杠累积
- 回收站路径硬编码 → `TRASH_DIR` 常量
- `cookie.parse().unwrap()` panic 风险
- PWA manifest shortcuts URL 错误

### Security
- `pinas-core/src/crypto.rs` 集中密码学函数，消除 `db→handlers` 反向依赖
- 分享过期时间检查统一使用 UTC

### Architecture
- 优雅关闭：SIGINT+SIGTERM 双信号 + `CancellationToken` 协调后台任务 + 5s 超时
- 媒体代理：HEAD 方法 + Range 拖拽 + `Content-Encoding: identity`

---

## v1.3.6 (2026-07-05)

### Changed
- SPA→HTMX 架构迁移：删除 `assets/js/*`（16 个 SPA 视图文件）
- 新增 Askama 模板系统（9 页面 + 21 组件）
- 错误处理统一为 `AppError` 枚举 + `AppResult<T>`

### Added
- HTMX 片段路由（约 30 条）
- `templates.rs` Askama→Axum 适配器
- `pages.rs` 页面处理器（`page_handler!` 宏）

### Fixed
- `#[serde(default)]` 配置字段反序列化
- `dotenvy` → 手动 `.env` 解析器
- `separator("_")` 导致配置键被拆分
