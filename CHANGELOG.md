# Changelog

## v1.5.1 (2026-08-04)

### Security（全面评估驱动,3 项 Critical 修复）
- **C1 任意文件写入修复**:merge_chunks 的 `file_name` 绕过沙箱(可写 `../../.env`、`~/.ssh/authorized_keys` 等,
  配合开放注册构成完整 RCE 链)→ 新增 `validate_name()` 白名单(拒绝路径分隔符/穿越/引号/尖括号/控制字符)
  + 合并目标 `starts_with` 兜底复检;`validate_name` 同时挂到建文件夹/重命名/移动
- **C2 存储型 XSS 通道封堵**(三条):
  - 模板内联 JS 字符串外置:file_table/breadcrumbs 的 onclick/hx-on 改 `data-*` 属性 + 事件委托,
    preview onerror 改静态 fallback(JS 内零用户数据)
  - 分享/媒体下载强制类型:html/svg/xml/js 一律 `application/octet-stream` + `Content-Disposition: attachment`
    (新增 `is_force_download_mime`);顺带修复分享页下载按钮指向"仅探测有效性"端点的功能 bug
  - Askama `serde_json` feature:`hx-vals` 全部改用 `|json` 过滤器(引号注入系统性修复)
- **C3(潜伏)API Key 窃取修复**:使用全局 DeepSeek key 时强制全局 api_base(用户不可自定义);
  settings 的 api_base 仅允许 https 域名(拒绝 IP/内网/本地)
- `safe_join_sandbox` 回退语义根治:5 处"攻击时返回 base"改为 `Err`(修复 `delete name=".."` 移走整个
  uploads 目录的漏洞),18 个调用点全部改造;`delete_batch`/`move_batch` 非法条目跳过而非回退
- CSP 清理死条目(unpkg/cloudflareinsights/ws/wss);`unsafe-inline`/`unsafe-eval` 保留并注明
  (Alpine/htmx 架构硬依赖)

### Fixed
- **自动备份损坏一个月**:VACUUM INTO 在备份文件连接上执行(自锁失败/产出空文件)→ 改在源库池连接执行
  + 保留 7 份轮转;抽 `auto_backup_once` 并加有效性测试
- 限速可伪造 X-Forwarded-For 绕过:新增 `MaybePeer` 提取器,直连(非回环)忽略一切客户端头,
  回环(cloudflared)信任 CF-Connecting-IP
- 分片阶段无配额/磁盘上限:upload_chunks 新增 `bytes_received`(迁移 v4),单用户临时分片 5GB 上限,
  merge 前先算总量预检配额(避免大文件写完才拒)
- 空文件媒体 Range 下溢(`(file_size-1)` → u64::MAX)→ 200 空体
- 明文密码进日志/credentials.txt:日志只打位置提示,首次登录成功自动删除凭据文件
- 错误细节回显客户端 4 处(agent/zip 压缩/move/回收站)→ 仅日志
- `download_zip` 临时文件:600s 延迟删除(慢链路会中途删文件)→ 流结束即删(DeleteOnDropFile)
- 会话 role 快照:中间件实时联查 users.role(降权立即生效)
- `/home/minecraft-status` 片段补 admin 角色校验;change_password 加限速(3 次/分钟)
- PDF 预览 embed → iframe(CSP object-src 'none' 会拦截 embed)
- 离线降级:SW 503 JSON 不再原文 swap 进容器(beforeSwap 拦截 + toast)
- zip 条目名净化(拒 "."/".." 防 zip-slip)

### Changed
- 注册开关 `PINAS_ALLOW_REGISTRATION`(默认关闭,生产未配置即不可注册)
- 配额算法统一:全量重算改 `SUM(CEIL(size_mb))`,与上传 ceil 增量语义一致(消除显示漂移)
- 上传秒传/断点续传真启用:identifier 改内容哈希派生(SHA-256 前 1MB+大小),同内容重传自动秒传
- 上传并发导航乱序防护:afterSwap 检测到过期列表响应自动以最新路径重发
- argon2 `0.6.0-rc.8` → `0.5.3` stable(哈希格式兼容,既有账号可直接登录);sha2 对齐 0.10 消除双版本
- SW v10:移除 marked/purify 死预缓存与 unpkg 死分支(全库无调用点,已核实)

### Cleanup
- 死表 `conversation_messages` 移除(迁移 v5);死代码清理(recalc_user_used_mb/count_user_files/
  file_exists_by_identifier/list_user_files/FileRow/DEFAULT_SESSION_DAYS/RANDOM_PASSWORD_LEN)
- jsdom 未使用 devDependency 移除;CLAUDE.md/CHANGELOG 同步

### Tests
- 新增 8 个测试:穿越 merge 拒绝(5 种载荷)、delete ".." 拒绝、非法名称创建/重命名、分享下载
  Content-Disposition、备份有效性、rename/move 子树路径迁移、媒体 Range 语义(HEAD/206/416/空文件)
- 集成测试文件系统隔离:test_app 将 CWD 切换到独立临时目录(不再污染项目 uploads/)
- 全量 30 个测试通过(11 lib + 19 integration),clippy 零警告

### Build & Ops
- 版本 1.5.1;部署脚本 `scripts/deploy.sh`(按 git HEAD 构建 + VERSION 记录 + 健康检查);
  版本对齐检查 `scripts/check-versions.sh`(模板 ?v= vs SW 预缓存,纳入构建流程)

---

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

### Performance
- Cloudflare 隧道协议 `http2 → auto`（QUIC 建立成功，隧道段无队头阻塞；曾 502 后改 http2 治标，
  TCP 被运营商限速导致 TLS 握手 5-15s、TTFB 波动 1-62s）
- 前端依赖全本地化：htmx/alpine 从 unpkg 移入 `assets/`（第三方 CDN 国内链路不可控）
- marked/purify 改为按页加载（仅 home/agent 的 `head_extra`，其他页省 2 请求 + ~66KB）
- 脚本加 `data-cfasync="false"`（Rocket Loader 异步化会破坏 htmx 同步语义）
- login/change_password 公开页加 `Cache-Control: public, max-age=60`
- SW v9：预缓存全部本地资源，移除 unpkg 依赖

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
