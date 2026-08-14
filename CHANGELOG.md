# Changelog

## v1.8.1 (2026-08-14)

### Security（审计驱动的关键修复）
- **WebDAV 认证缓存绕过修复（C1, Critical）**：AUTH_CACHE 仅按 username 放行，60s 窗口内
  任意密码可通过认证 → 缓存键值改为 sha256(username\0password) 凭证指纹，命中必须指纹一致
- **WebDAV 认证限速（H4）**：/dav 公开无限制，Argon2 ~100ms 可被公网 CPU DoS/爆破
  → 未命中缓存（必须跑 argon2）的尝试按对端 IP 限速（复用登录限速 10 次/60s；
  成功路径 60s 内走缓存不消耗额度）
- **WebDAV PUT 覆盖原子化（H3）**：先删旧文件再 rename，且内容策略校验在 rename 之后，
  策略失败/崩溃即永久丢失旧文件 → rename(2) 直接原子替换；扩展名+MIME 校验前置到 tmp；
  覆盖走 UPDATE 保留行元数据；配额复核按增量（旧大小释放）
- **MIME 黑名单补全**：infer 0.19 对 Windows EXE/DLL 报
  application/vnd.microsoft.portable-executable，原黑名单（x-executable/x-sharedlib）
  拦不住改名穿透 → 补 PE/wasm/mach

### Fixed（数据完整性）
- **分片磁盘上限失效（H1）**：File::create 截断后才读旧大小，bytes_received 恒 0，
  5GB 防耗尽上限形同虚设 → 截断前读取；写失败/超限/空流/MIME 阻断路径回滚计数
- **分片临时目录按用户隔离（H2）**：uploads/tmp/{identifier} 全局共享，可窃取/污染他人
  未合并上传 → tmp/{username}/{identifier}；清扫任务下钻一层逐个判定（防用户名目录 mtime
  失真误删活跃分片）
- **WebDAV COPY 事务解耦（H5）**：磁盘复制包在 SQLite 写事务内 + std::fs 阻塞 async worker，
  大目录 COPY 冻结全站写操作 → 磁盘复制移出事务并 spawn_blocking，DB 登记走短事务，
  失败回滚并清理已复制目标
- **dsh 反代凭据隔离（H6）**：浏览器 Cookie/Authorization 原样透传上游（admin 会话令牌
  暴露给独立应用）+ 上游 Set-Cookie 可污染同注册域会话 → 转发前剥除
  Cookie/Authorization/Proxy-Authorization/X-Forwarded-*/X-Real-IP，响应剥除 Set-Cookie

### Tests
- 集成测试 57 → 62(+5)：dav 缓存凭证绑定、dav 限速 429、PUT 覆盖策略失败保旧文件、
  bytes_received 累计与重传不重复计数、分片目录跨用户隔离

## v1.8.0 (2026-08-14)

### Added
- **DeepSeek Harness 反代入口**：第二监听 127.0.0.1:3100（`PINAS_DSH_PORT`），把
  `pidsh.antifield.work`（cloudflared 本地接入）经 pinas admin 会话认证后全量 HTTP/WS 转发到
  本地 dsh（127.0.0.1:3080）。WS 仅放行事件下行路径 `/api/events.mux`、`/api/events.host`；
  dsh 配置平面特权方法（settings/credentials 等 15 项）转发时剥 Origin 并回指环回 Host
  （对齐 dsh 环回信任栅栏）；未登录经 dsh 域访问时 302 至 drive 登录页（带 redirect）。
- **统一登录**：`PINAS_COOKIE_DOMAIN`（如 `antifield.work`）让 drive/dsh 同注册域共享会话
  Cookie；导航栏与首页新增 Harness 入口（仅 admin 可见，新标签打开）。
- dsh 集成测试：admin-only、Host 注入、坏网关 502、cookie Domain 门控。

## v1.7.3 (2026-08-13)

### Changed
- **修改密码改为弹出式**：导航「账号设置」不再跳独立页，改为 HTMX 片段注入模态框
  （新路由 /account/password-form + components/password_change_form.html）；
  成功即关闭弹窗 + Toast，无需离开当前页面。强制改密流程仍走独立页 /change-password
- 移除「账号设置」按钮的 emoji 符号

## v1.7.2 (2026-08-13)

### Added
- **账号设置入口**：导航栏新增「🔑 账号设置」（桌面 + 移动端），直达 /change-password 修改密码
  ——此前该页面仅在强制改密（首登/管理员重置）时可达，用户无法主动更换密码
- 改密页文案适配主动场景 + 「← 返回」链接

## v1.7.1 (2026-08-13)

### Performance
- **WebDAV 阻塞 IO 治理**:PUT 流式写盘循环用 std::fs 占死 async worker(1GB 慢速上行可冻结全站)
  → tokio::fs + 绝对路径基准(根治历史 ENOENT 竞态);PROPFIND 元数据查询同改异步
- **列表热路径去 N+1**:逐行 stat+DELETE(每行一个 WAL 写事务) → 并发 join_all + 单条批量 DELETE;
  HTMX 片段列表补 1000 行防御上限
- **配额增量调整**:update_user_used_mb 全表 SUM 扫描 → adjust_user_used_mb(_tx) 增量助手,
  编辑器保存/merge 提交路径接入(对账保留)
- **FTS5 大小写不敏感**(迁移 v9):搜 report 命中 Report.pdf(重建虚拟表 + 触发器 + rebuild)

### Fixed / Hardening
- **迁移体系加固**:25+ `.expect()` + panic=abort 意味着一次 DDL 失败整站启动崩溃
  → 全部 Result 化 + 整批迁移单事务(失败整体回滚,main 日志报错退出)
- **interval(0) 启动崩溃**:临时分片/回收站清理间隔为 0 时 panic → 钳位 ≥1
- **限速 map 满时驱逐受害者**:攻击者可挤掉目标计数重置窗口绕过爆破防护 → 满时拒绝新键
- **Mutex 中毒炸站**:dav AUTH_CACHE 中毒 unwrap → panic=abort → into_inner 恢复
- **HSTS 误伤局域网**:无条件下发使纯 HTTP 直连被浏览器永久升级 → 仅 X-Forwarded-Proto:https 时下发
- **配额缺口补齐**:回收站恢复超配额拒绝、编辑器保存按增量检查、WebDAV COPY 配额进写事务
- **对话历史无界增长**:读取 LIMIT 500 + 每小时保留任务
- 小修批量:create_folder 重复 409(不再孤儿目录+误导 500)、LIKE 通配符转义(ESCAPE '\')、
  416 补 Content-Range、delete_batch 如实报告部分失败、bytes_received 重传重复计数修正、
  rename_conversation 越权 404、WebDAV PUT 内容策略(扩展名黑名单+MIME 检测)、
  admin 片段 403(不再 200 空数据)、日历时区统一 Local、credentials.txt 兜底清理、
  agent model 白名单校验、todo_form Alpine 值迁移 data-* 属性、page_context 去闭包
- **Service Worker v12**:/api/ 响应不再缓存(敏感 JSON 滞留 CacheStorage + 离线静默陈旧数据)、
  activate 清空运行时缓存、manifest 入预缓存清单(?v=1)、日志串 v9→v12 修正

### Deployment
- 清理 ~/pinas 旧 SPA 遗留文件(app.js、js/、tailwind.js、static/index.html)
- systemd unit Description 同步 v1.7.1

## v1.7.0 (2026-08-13)

### Security（审计驱动的关键修复）
- **媒体令牌替代 URL 会话凭证**:`/api/media/?token=` 携带完整会话凭证(进日志/历史/分享链接即泄露账号)
  → 新增 media_tokens 表(迁移 v8),预览页签发 30 分钟短时效、目录路径限定的 `mt` 令牌;旧 `?token=` 一律拒绝
- **对话越权写入修复**:save_chat_round 无归属校验,任何登录用户(含 guest)可向他人对话注入消息
  (历史进入 LLM 上下文即提示注入)→ 新增 assert_conv_owned,写路径强制校验
- **AI 端点双重限速**:agent 全部端点无限制(guest 可烧光全局 DeepSeek 额度)
  → 每用户 5 分钟窗口限速 + 每日配额(PINAS_AGENT_DAILY_QUOTA,默认 200)
- **分享端点匿名防护**:分享页/下载/子文件无认证无限制(Argon2 CPU DoS + 无限速爆破)
  → 每 IP 限速 + 每分享 5 次失败锁定 15 分钟
- **Cookie 默认强制 Secure**:原仅在 X-Forwarded-Proto:https 时设置,直连 HTTP 凭证明文传输
  → 默认 Secure(纯 HTTP 局域网场景显式 PINAS_COOKIE_SECURE=false)
- **用户枚举时序侧信道**:未知用户立即返回 vs 已知用户 Argon2(~100ms)→ 登录与 WebDAV 均哑哈希等时校验
- **链接收藏 scheme 校验缺口**:HTMX 片段路径可存 `javascript:` URL(点击即执行,存储型 XSS)
  → 共享 validate_url(http/https + 主机名),四条写路径统一
- **密码不再打印**:PINAS_ADMIN_PASSWORD 掩码前缀曾进 journald → 只打印已设置/未设置
- **环境变量密码不再每启重置**:sync_user_password 每次重启用 .env 覆盖 DB 密码(UI 改密被悄悄回滚)
  → PINAS_SYNC_PASSWORDS=true 显式开启(默认关,首次运行不受影响)

### Fixed（数据完整性）
- **分片空洞产出损坏文件**:完整性检查只数数量,{0,1,2,4} 能通过 → 严格集合相等 + DB 错误传播
- **WebDAV MOVE 覆盖失败目标隐身**:位移目标 DB 行先删、失败只还原物理文件 → 行暂存,失败完整回滚
- **回收站清空只删行不删文件**(HTMX 片段路径)→ 与 JSON API 共用物理删除
- **带时间日程在日历/日期筛选隐身**:`2026-08-13T10:00:00` 与 `2026-08-31` 字符串比较失败
  → 统一 substr(due_date,1,10) 比较与计数
- **SSE 多行 delta 帧损坏**:含 \n 的 Markdown 内容静默丢失 → json_data 编码 + 客户端 JSON 解码兜底
- **对话断连丢历史**:持久化依赖流尾元素,客户端断开即取消 → detached task 独立跑完上游并持久化
- **AI 截断输出炸站**:模型输出尾部 `args=` 时切片越界 panic(panic=abort 整站宕机)→ get() 安全切片
- **登录 ?redirect= 开放重定向**:javascript:/https:// 站外跳转 → 客户端仅允许 `/` 开头路径
- **媒体加载失败 fallback 死代码**:showMediaError 未挂全局 → 补 window 别名
- **全局搜索排序丢搜索词**:排序表头 hx-vals 缺 search → 表头携带当前搜索词
- **page_size 非正数无界返回**:负 LIMIT 在 SQLite 意为无限制 → clamp(1, MAX)
- **WebDAV 认证阻塞 async 运行时**:Argon2 直接跑在 worker → spawn_blocking
- Dockerfile 修复(pinas-core 已合并、缺 templates/ 编译期必需);deploy.sh 补拷 JS 库;
  CI 增加 check-versions/aarch64 检查/docker 冒烟

### Tests
- 集成测试 33 → 48(+15),单元测试 12 → 17(+5):同名重传 409、重命名覆盖保护、
  中文多字节子树迁移、回收站清扫豁免、FK 全连接强制、媒体令牌作用域/过期、
  分享爆破锁定、AI 配额、SSE 截断解析、对话归属、带时间日程日历等

## v1.6.1 (2026-08-13)

### Fixed（P0 数据丢失与关键安全）
- **回收站被 24h 临时清扫销毁**(生产已发生 1 例):TRASH_DIR 位于 uploads/tmp 内
  → 迁至 uploads/.trash(启动迁移,先于清扫任务)+ 清扫器白名单(仅分片/dav 临时条目)
- **重传同名文件销毁旧文件**:merge File::create 先截断,INSERT 冲突后清理守卫连旧文件一起删
  → 同名预检 409
- **重命名/移动覆盖销毁目标**:fs rename 原子替换,DB UNIQUE 冲突回滚后目标内容已丢
  → rename_core/move_core 目标存在预检 409(AppResult 化,6 调用点传播)
- **中文目录深层子路径损坏**:SUBSTR 偏移用字节长度(字符语义)+ LIKE 未转义 %/_
  → SQLite length(?) + escape_like
- **模型截断输出触发 panic=abort 整站宕机**(parse_text_invokes)→ 安全切片
- **PRAGMA foreign_keys 仅单连接生效**:per-connection 设置 → SqliteConnectOptions::foreign_keys(true)

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

## v1.6.0 (2026-08-04)

### Added — AI 体验
- **SSE 流式输出**:`POST /api/agent/chat/stream`(DeepSeek stream:true 透传,打字机效果;
  reqwest 加 `stream` feature + 300s 单请求超时;流终结时持久化整轮对话,工具消息不入库)
- **工具调用(function calling)**:5 个工具(搜索文件/读文件(≤1MB,提示注入防护标注)/查待办/建待办/系统状态(仅管理员)),
  最多 5 轮工具循环,工具错误回传模型可修正;`DeepSeekMessage` 扩展 tool_calls/tool_call_id/name,
  OpenAI 兼容 tools schema
- **AI 回复 markdown 渲染**:marked v15 + DOMPurify 重新接入(本地资源,sw.js 预缓存三方对齐),
  历史消息与流式回复统一渲染(DOMPurify 消毒防 XSS)
- agent.html 表单改 fetch + ReadableStream 流式解析(HTMX 不支持 POST SSE);发送中禁用按钮 + "思考中…"指示

### Added — 文件体验
- **全局搜索(FTS5 trigram)**:迁移 v7 建 `files_fts` 外部内容表 + 3 同步触发器(INSERT/UPDATE/DELETE)+ rebuild;
  搜索词 ≥3 字符走 FTS 子串匹配(中英文),≤2 字符降级 LIKE 兜底(trigram 限制,实测验证);
  drive 页"全局"checkbox,结果跨目录显示路径,点击跳转退出搜索态
- **上传队列 + 文件夹上传**:`window.UploadQueue` 并发 3 调度 + 右下角进度面板(单文件状态/总进度/取消);
  拖拽 `webkitGetAsEntry` 递归遍历目录(保留相对路径)、"上传文件夹"按钮(webkitdirectory);
  上传到未登记子目录自动补插目录行(`ensure_dir_rows`,幂等)
- **预览补强**:markdown 渲染分支(`serde_json` 编码 + `<` 转义防 `</script>` 逃逸);
  视频续播(localStorage 记忆进度,5s 节流,结尾不恢复);图片画廊上下翻页(同目录相邻)

### Added — WebDAV (`/dav/`)
- 全平台同步客户端入口(Rclone/RaiDrive/手机文件管理器/Windows 映射):PROPFIND(Depth 0/1, infinity→403,
  手写 XML + RFC 4331 配额属性)/GET+Range(无 Range 全量)/PUT(流式落盘 temp + 原子覆盖 + 配额预检复核)/
  MKCOL(父目录须存在,重复 405)/MOVE(Destination 解析,改名+跨目录,Overwrite:F→412)/
  COPY(深树递归 + 配额累加)/DELETE(进回收站可还原)/LOCK 伪实现(单写者场景)/OPTIONS 免认证
- Basic 认证(60s 成功缓存防每请求 argon2,角色实时查;`must_change_pwd` 拒绝);
  路由级 5GiB body limit 覆盖全局 100MB;文件名 URL 编码 href
- 注:dav.rs 文件操作统一 std::fs(测试环境 tokio::fs 相对路径 ENOENT 竞态,同步调用稳定)

### Fixed (生产冒烟发现)
- 工具调用文本格式兼容:DeepSeek V4 系列默认输出 `<invoke name="...">` 文本调用(非 OpenAI
  结构化 tool_calls)→ 新增 `parse_text_invokes`(块体 + args 属性两种形式),执行结果以
  `<invoke-result>` 包裹回传,继续工具循环(3 个单元测试)

### Changed
- 依赖:reqwest 加 `stream` feature,新增 `futures-util`/`base64`
- `move_core`/`rename_core`/`delete_to_trash`/`parse_range` 提 `pub(crate)`(WebDAV 复用)
- `bind_list_where`/`query_files` 全局搜索分支;`list_files` 全局搜索按行内 parent_path 校验磁盘
- system.rs 抽 `collect_system_metrics`(状态接口与 AI 工具共用)
- sw.js v11(precache 加 marked/purify),模板 `?v=` 三方对齐(check-versions.sh 通过)

### Tests
- 新增 13 个集成测试(44 总):WebDAV 全链路(401/回读/覆盖/目录行/MOVE+Overwrite/COPY/DELETE 回收站/
  Range/MKCOL/配额 507)、全局搜索(中文 3 字 FTS/2 字 LIKE/ASCII)、嵌套目录 merge 补插、
  markdown 预览转义、AI 流式未配置 503

### 遗留
- 离机备份(挂起事项)与文件历史版本未纳入本期
