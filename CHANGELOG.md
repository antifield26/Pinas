# Changelog

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
