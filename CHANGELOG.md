# Changelog

## v1.12.0 (2026-08-17)

### Fixed（P0 安全修复——跨用户文件越权，code_review.md 实证）
- **P0-1 写路径 `..` 未拦截 → 跨用户文件操作**：rename/move/delete/merge 的 parent/src/dst
  仅经 trim 透传，多用户共享 uploads/ 根沙箱 + openat2 BENEATH 允许根内 `..` 解析，
  任意登录用户可移动/删除/窃取他人文件（隔离测试实证：move 200 窃取成功、delete 200 成功）
  - 新增 `validate_rel_path`（utils.rs）：逐段拒绝 `..`/`.`/空段/反斜杠/控制字符，返回规范化相对路径
  - `rename_core`/`move_core`/`delete_to_trash`/`merge_chunks` 入口统一校验 parent/src/dst；
    API/HTMX/JSON 入口显式校验返回 400
- **P0-2 rename/move 源名未校验**：`old_name`/`name` 补 `validate_name`（`..` 源名可移走整棵
  用户子树，物理/DB 错位后 reconcile 大规模误清）
- **回归测试**：新增 `tests/path_validation.rs`（7 项：跨用户 move/rename/delete/merge 拒绝 +
  源名 `..` 拒绝 + 同用户正常操作不受影响）

### Fixed（P1 高优先级）
- **分享目录路径泄露**：目录分享 JSON 的 `path` 裁剪为相对分享根（此前回吐 uploads 内部路径
  与拥有者用户名，匿名访客可枚举）
- **分享失败锁定可 DoS**：按 `IP|code` 组合键计数（此前按 code 跨 IP 共享，任意 IP 错 5 次
  即锁死合法分享 15 分钟）
- **CSP 第二哈希失配**：theme_head.html 暗色预涂脚本哈希更新为实测值
  `sha256-V1ONiGmI3S/fo/iVTzDdp1MvwZTNDqZJWdZu1ok6Ees=`（此前 /login、/change-password、
  /s/* 预涂脚本被 CSP 拦截，白闪）；`check-versions.sh` 新增「内联脚本→sha256」自动校验防再漂移
- **dsh 路由认证边界**：`auth_middleware` 的 `/s/` 豁免与媒体令牌允许改为 `AuthPolicy` 显式声明
  （主路由放行，dsh 路由全禁）——dsh 域未认证 `/s/*` 实测 500（潜在绕过）修复为 401 fail-closed
- **分片 5GB 上限 TOCTOU**：预检 + 预留并入同一 SQLite 写事务（抢写锁串行快照），写盘后按实际
  校正（-prev -MAX +actual）、失败撤销预留——并发绕过 5GB 上限的通道关闭
- **VACUUM 阻塞 async worker**：备份移入 `spawn_blocking`（闭包内新建独立连接）+ 备份互斥信号量
- **DAV 位移/恢复校验**：Destination 目的父路径过 `validate_rel_path`（源侧同补）；
  `journal::recover_dav_disp` 对 username/parent/fname 逐项校验 + 首段断言（崩溃恢复期跨用户
  写入通道关闭）
- **回收站清空/永久删除非事务化**：物理失败不再吞错删行（磁盘孤儿）；清空改「先物理全部成功
  再单事务批量删行」；`delete_to_trash` 的 trash 登记 + files 删除包入单事务
- **DAV 无 Content-Length PUT 写盘放大**：chunked PUT 落盘路径补 PENDING 约束校验与日志

### Changed（P2 改进）
- 限速器按条目自身 window 清理（跨端点键耗尽 DoS 关闭）；仅信任 `cf-connecting-ip` 且校验
  IP 格式（伪造 XFF 清零限速关闭）
- 登录响应 token 仅回显给显式 `Authorization: Bearer` 请求方（浏览器走 HttpOnly Cookie，
  页面侧 XSS 拿不到 token）；change_password 会话校验与中间件双闸一致（空闲超时生效）
- 媒体令牌路径比对改为百分号解码后逐段比较（与 handler 语义对齐）
- 分享下载链接用一次性令牌（迁移 v13 `share_tokens` 表）替代明文密码进 URL
- `reconcile_files_on_disk` 复用单一 Sandbox；download_zip 存在性判定走沙箱原子操作
- 请求 ID 截断 ≤128；随机密码拒绝采样消取模偏差；config.rs 移除 v1.11 残留键名；
  main.rs 注释移除"AI 助手"；admin_users.html 补 `|json`；deploy.sh 失败自动回滚 +
  `-D warnings` 构建；check-versions.sh 扩展懒加载库版本校验；cargo-deny-action pin SHA
  （修正为真身 EmbarkStudios）
- 回收站后台清理与还原互斥（per-uuid 锁表）；分片目录清扫豁免 DB 存活记录

### Tests
- 集成测试 77 → 84（新增 7 项 P0 回归）；单元测试 15 → 18（限速器/回收站锁/临时清扫豁免）
- schema 版本 v12 → v13

### Removed（内置 AI Chat 移除，AI 能力收敛到 dsh）
- **代码**：handlers 删除 agent.rs / conversations.rs / settings.rs（含 validate_api_base）；
  core::secrets.rs（主密钥 + ChaCha20-Poly1305）一并移除；依赖移除 chacha20poly1305
- **路由**：`/agent` 页面、`/api/agent/*`（chat/chat/stream/briefing/models/settings）、
  `/api/conversations*`、`/agent/*` HTMX 片段全部删除；导航栏 AI 入口（templates.rs nav_items）
  与 PAGE_AGENT 常量移除
- **模板/前端**：agent.html + 5 个组件（chat_message/conversation_list/settings_form/
  briefing_result）、todos 页"AI 简报"按钮；app.js 的 AppAgent 命名空间（SSE 流式/会话管理/
  设置表单/事件委托）整体清理——保留 renderMarkdown（.md 预览共用）与 marked/purify 懒加载
- **配置**：PINAS_DEEPSEEK_* / PINAS_AGENT_DAILY_QUOTA / PINAS_MASTER_KEY 移除
- **数据清理（迁移 v12）**：DROP user_settings（含已加密的 API Key）/ conversations /
  conversation_messages；历史迁移 v3（user_settings 加列）同步移除；
  生产 secret.key 主密钥文件删除、.env 无 AI 变量残留
- **PWA**：manifest 移除 AI 快捷入口与描述；SW v16 / app.js?v=2 / manifest?v=2
  （Cache First 策略下内容变更强制换缓存键）

### Kept（保留项）
- dsh 反代（127.0.0.1:3100，admin 会话门禁）与首页 DeepSeek Harness 跳转卡片
- dsh-plugin-pinas 依赖的功能 API（files/todos/links/system/edit 生产实测全通）

### Tests
- 集成测试 81 → 77（AI 相关 4 项删除，FK 测试改用 todos 表）；15 单元全绿

## v1.10.0 (2026-08-16)

### Security（P0 残余风险根治）
- **DNS 重绑定钉扎（P0-1）**：AI 请求前真实解析 api_base 域名，任一结果落在
  私网/环回/链路本地/CGNAT/文档网段即拒绝；客户端按 base 缓存 10 分钟并 `resolve()`
  钉扎连接 IP（SNI 仍为域名，证书校验不变）；禁止代理绕过钉扎
- **会话空闲超时（P0-2）**：schema v11 sessions.last_active_at；绝对过期（7 天）+
  空闲超时（默认 24h，PINAS_SESSION_IDLE_MINUTES）双闸；中间件惰性刷新（≥5 分钟
  才写库），后台任务定期清理超空闲会话
- **AI Key 落库加密（P0-3）**：ChaCha20-Poly1305（core::secrets），格式
  `enc:v1:base64(nonce‖ct‖tag)`；主密钥 = PINAS_MASTER_KEY 或 data_dir/secret.key（0600
  自动生成）；旧明文读取兼容、下次保存即升级；密钥文件损坏拒绝启动（防密文不可解）
- **符号链接 TOCTOU（P0-4）**：新增 fsutil::Sandbox = openat2(RESOLVE_BENEATH |
  NO_MAGICLINKS) + renameat/unlinkat/mkdirat/statat 族，全部物理文件操作迁移
  （media/file_ops/dav/share/trash/journal/upload/agent）；内核级原子解析，越界
  （含绝对路径链接）一律拒绝；字符串校验 safe_join_sandbox 保留为纵深防御第一层

### Engineering（P1 工程治理）
- **模块拆分**：file_ops.rs 1796 行 → file_ops/{mod,core,api,fragments}；
  dav.rs 1351 行 → dav/{mod,auth,ops}（行为零变化，80 测试回归通过）
- **X-Request-Id（P1-2）**：全部响应携带请求 ID（沿用入站值），tracing span 注入
  request_id 字段；dsh 反代读取扩展注入上游请求头（跨进程链路贯穿）
- **供应链（P1-3）**：deny.toml（advisories + yanked + 15 项宽松许可白名单）；
  CI 增加 cargo-deny-action；Dependabot（cargo 每周 + actions 每月）；
  Cargo.toml 补 license 字段；spin 0.9.8 → 0.9.9（yanked）
- **配置脱敏（P1-4）**：PINAS_DEEPSEEK_API_KEY / PINAS_MASTER_KEY 启动日志掩码
- **文档（P1-5）**：CLAUDE.md 去漂移（结构图/测试计数/环境变量/已知边界）

### Tests
- 新增符号链接越界（读/写路径 4 项）、DNS 私网 IP 分类、密钥加解密往返、会话空闲超时、
  X-Request-Id 等；25 单元 + 81 集成全绿

## v1.9.1 (2026-08-15)

### Fixed（链接库移动端布局）
- 双列排布在移动端无法完整显示标题 → 单列全宽（grid-cols-1 sm:grid-cols-2 lg:grid-cols-3）
- 标题两行截断（line-clamp-2 + break-words）
- 行内操作改图标按钮（icon-btn），修复 `<a>` 嵌套 `<button>` 的非法 HTML 结构

## v1.9.0 (2026-08-15)

### UI（视觉与动画优化）
- **设计令牌**：缓动曲线/时长/交错步长 token 化（--ease-out-soft/--dur-*），动画统一引用
- **组件类收敛**：btn（primary/secondary/ghost/danger/sm）、icon-btn、form-label、input-error、
  badge 四色、empty-state、card-hover、row-hover——形状/交互/焦点环/按压反馈全站统一
- **简约 SVG 图标系统**：partials/icons.html 提供 ~40 枚线性图标（24×24/stroke1.5/currentColor），
  文件类型图标按扩展名映射（Rust 侧 icon_kind）；替换全部 emoji（🌓☰📁📄✕✓🎬✎♪）
- **骨架屏**：系统监控/MC 状态/文件列表首载 shimmer 占位，替换纯文本 animate-pulse
- **Toast 升级**：类型图标（alert/check/info）+ 左侧色条 + 4s 自动关闭倒计时条
- **聊天体验**：AI 渐变头像块、流式光标（▍ blink）、思考中指示、usage 弱化
- **批量工具栏**：显现淡入（hidden 硬切换 → animate-fade-in 重放）
- **View Transitions（渐进增强）**：hx-boost 整页导航在支持时用浏览器原生交叉淡化
  （reduced-motion/不支持自动回退 CSS 转场）
- **配额条**：>90% 红色 / >70% 琥珀警示态；空状态统一组件；表单标签/错误态规范化
- **prefers-reduced-data**：骨架扫光/进度条纹关闭

## v1.8.4 (2026-08-14)

### Security（遗留审计项收口）
- **CSP 全收敛（H8）**：script-src 移除 'unsafe-inline'——全部内联脚本外置到 assets/app.js
  （hx-boost 导航天然只执行一次，H7 守卫语义固化）；全部内联事件处理器（onclick/onsubmit/
  onchange/oninput/onerror 等约 30 处）改为 data-* 属性 + document 事件委托；仅保留两处
  head 主题预涂脚本（防闪白必需），经 CSP sha256 哈希放行（'unsafe-eval' 保留：Alpine
  表达式与 htmx hx-on 依赖）
- **Argon2 参数上调**：t=2（OWASP 下限）→ t=3（RPi 4 核 ~150ms），爆破成本提升 50%；
  验证参数随哈希串自描述，旧哈希不受影响
- **api_base 深度 SSRF 校验**：统一 validate_api_base（https-only + URL 解析 + 拒 IPv4/IPv6
  字面量、私网/链路本地前缀、.local/.internal、nip.io/sslip.io/xip.io/localtest.me 重绑定后缀），
  写入与读取（resolve_agent_config）双侧执行；残余重绑定风险文档化
- **WebDAV 认证缓存即时失效**：改密/管理员重置后旧凭证不再命中 60s 窗口
- **媒体令牌前缀规范化**：签发时去空段/拒绝 ..（路径限定校验的歧义面）
- **dsh 反代资源治理**：WS 并发上限 32（Semaphore）+ 上游 TCP 连接 3s 超时 +
  HTTP 读空闲 600s 上限（SSE keep-alive 不受影响）

### Fixed（数据与配额精度）
- **配额原子化（M3/M4）**：新增 check_and_adjust_quota_tx（事务内预检+增量调整），
  dav PUT / 回收站恢复 / 编辑器保存接入——消除「先检查后写」TOCTOU；
  update_user_used_mb 事务化（先空写抢锁再 SUM），全量重算与增量调整不再互相覆盖漂移；
  dav PUT 覆盖按 CEIL 差值计（旧大小释放）
- **PROPFIND 精确字节（M6）**：getcontentlength 用磁盘真实长度，不再 size_mb 反算
  （WebDAV-PUT 文件被放大最多 ~1MiB，rclone 校验卡 EOF）
- **COPY 记账一致（L3）**：逐文件 CEIL 后求和，与 upload 路径口径一致
- **假秒传修复（L5）**：check 携带目标 file_name/parent_path，内容曾存在于其他路径时
  不再报 exists（历史上传"成功"但目标文件从未创建）
- **列表 reconcile 限流（L7）**：join_all（1000 行目录瞬时千并发 stat）→ buffer_unordered(64)
- **sqlite:// 路径解析（L9）**：strip 后去前导 /，孤儿 WAL 清理路径判断失准修复
- **迁移降级检测（L10）**：数据库版本高于二进制时显式报错退出，不再静默运行
- **FTS rebuild 失败可观测（L11）**：v7/v9 迁移 rebuild 失败改 warn 日志
- **chunk rows 清理对齐（M12）**：孤儿分片行清理阈值随 PINAS_TEMP_CLEANUP_HOURS，
  不再固定 -1 day
- **对话历史清理优化（L12）**：相关子查询 O(rows×500) → 窗口函数 ROW_NUMBER 单次排序

### Fixed（AI 与系统集成）
- **search_files LIKE 转义（L4）**：%/_ 不再意外全匹配/误匹配
- **AI 参数 clamp 统一（L2）**：resolve 侧 clamp temperature/max_tokens，DB 异常值不直传上游
- **每日配额本地日界（L5）**：UTC（北京早 8 点重置）→ Local；键由清理任务定期回收
- **系统提示缓存主动失效（L9）**：待办/链接写路径调用 invalidate_prompt_cache，
  30s TTL 内改完待办立即问 AI 不再拿到旧上下文
- **MC 状态缓存与退避（I4）**：成功 15s 缓存；失败指数退避（4s 起封顶 5min），
  宕机期不再每 5s 一次 ~7s 的无意义 TCP 握手

### Fixed（运维）
- 日志轮转加 30 份上限（异常日不再无限堆积）；审计保留 90 天说明补全

### Tests
- 集成测试 70 → 76(+6)：超配额覆盖保旧文件、PROPFIND 精确字节、假秒传、改密后缓存失效、
  api_base 深度校验、CSP/无内联处理器回归

## v1.8.3 (2026-08-14)

### Fixed（前端与运维卫生）
- **hx-boost 脚本重执行守卫（H7）**：整页导航把 base.html 内联脚本随 innerHTML 重复执行，
  document.body 事件监听器层层累积（重复请求/重复 toast/重复模态逻辑）
  → window.__antifieldInitOnce 一次性守卫，全局初始化只执行一次
- **表单错误不再静默吞掉**：drive 建夹/重命名/移动/删除失败恒返回 200 列表 + 无差别关弹窗，
  用户看到"操作成功"假象 → 失败返回列表 + HX-Trigger toastError（base.html 弹错误 Toast），
  建夹片段同步 M11 INSERT 先行语义（同名 409 提示）
- **check-versions.sh 正则修复**：字符类不含 / 与 json，CSS/manifest 校验形同虚设
  （login/change_password/share 曾漂移 ?v=16 未被 CI 拦截）→ 含嵌套路径 + manifest；
  统一 ?v=18（重建 CSS 补 sr-only）
- **PWA SW v13**：预缓存公开壳 /login 取代 /（已登录 dashboard 的用户名曾被烤进
  CacheStorage）；移除 RUNTIME_CACHE 死代码；503 兜底统一 text/plain（base.html 按状态码处理）
- **marked/purify 懒加载**：两库合计 ~70KB 全站每页加载，仅 AI 聊天与 .md 预览需要
  → App.renderMarkdown 首次调用动态注入，就绪前纯文本占位、加载完成统一回填
- **部署纪律**：deploy.sh 脏树默认拒绝（--allow-dirty 显式豁免）、覆盖前自动备份
  pi_nas.bak.pre-{版本}、systemd unit Description 随版本同步（daemon-reload）；
  历史事故：未提交 dsh 反代以旧版本号上线、VERSION 与二进制对不上
- **/health 缓存**：公开端点每请求探 DB 可被免费放大 → 5s TTL 结果缓存（失败不缓存，
  时间戳每响应刷新）
- **logout 配置韧性**：Set-Cookie .parse().unwrap() 在 cookie_domain 配置异常时
  panic=abort 整站崩溃 → 降级为空 Cookie 不崩溃
- 可访问性：login/change_password/share 输入框补 sr-only label（WCAG 表单关联）
- extract_ip 信任边界写入文档注释（回环=本地进程信任域；多租户时需收敛）
- 文档同步：CLAUDE.md 的隧道主机名（cloud/pidsh/mc）、部署方式（systemd 直跑）、
  fs_journal 表、PWA 离线声明、marked/purify 懒加载

## v1.8.2 (2026-08-14)

### Fixed（数据与集成，审计驱动）
- **文件操作意图日志（M1）**：rename/move/delete 的 FS 与 DB 两步非原子，崩溃产生孤儿文件/
  幽灵记录且孤儿永不恢复 → 新增 fs_journal 表（迁移 v10）：物理操作前落意图，完成后删除；
  启动时重放（FS 完成补 DB / FS 未动重做 / 双方缺失告警），单条失败保留待下次重试。
  覆盖 rename_core/move_core/move_batch/delete_to_trash（dav MOVE 复用同一核心自动受益）
- **WebDAV MOVE 覆盖位移恢复（M9）**：位移目标存 uploads/tmp 会被 24h 清扫销毁 →
  迁至 uploads/.dav_disp + JSON 元数据（目标路径 + 暂存 DB 行），启动任务
  recover_dav_disp 按现场还原（覆盖完成则清理，中断则还原文件 + 重建 DB 行）
- **ZIP 打包上限与符号链接防护（M2）**：无总量/条目上限 + is_file/is_dir 跟随符号链接
  （递归环/反向 zip bomb）→ 2GB/1 万条目预算逐条扣减，symlink 一律跳过（含顶层与递归）
- **分片完整性校验（M3）**：断点续传只校验索引连续，截断分片被静默合并成损坏文件 →
  upload_chunks.chunk_sizes 记录每片实际字节，merge 逐一比对，损坏即拒绝并提示重传
- **merge 配额顺序（M10）**：先删分片后复核配额，超配即永久丢失分片 → 复核（事务内）前移，
  超配/MIME 拒绝只删目标文件、保留分片供清理空间后重试
- **建夹并发竞态（M11）**：预检→建目录→INSERT 的顺序下，后到者冲突后删掉先到者刚建的目录
  → INSERT 先行（UNIQUE 冲突即 409，绝不触碰目录），建目录失败回收 DB 行
- **删除子路径 LIKE 转义（M5）**：文件名含 %/_ 时 delete_to_trash 误删兄弟目录 DB 行 →
  child_prefix 全段 escape_like + ESCAPE '\'（与 dav/update_child_paths 对齐）
- **AI 工具结果结构化回注（M6）**：文本 <invoke> 的工具结果以 role="user" 回注（权威过高，
  恶意文件内容可诱导二次工具调用）→ 转为结构化 assistant.tool_calls + role="tool"
  （合成 tool_call_id，与结构化路径语义一致）
- **AI 配额按真实调用计费（M7）**：一次工具循环最多 6 次上游调用只计 1 次额度 →
  循环内与最终流式调用逐次 agent_check_rate
- **SSE 响应加固（M8）**：补 Cache-Control: no-cache + X-Accel-Buffering: no +
  Connection: keep-alive；压缩谓词豁免 text/event-stream（防边缘缓存/gzip 缓冲破坏流式）
- **SSE 分帧兼容 CRLF（M14）**：上游用 \r\n\r\n 分帧时 data 行解析恒失败（零事件 + 空消息持久化）
  → 同时匹配 \n\n 与 \r\n\r\n
- **统一登录回跳修复（M12）**：drive 登录页拒绝绝对 https redirect，dsh 域登录后落到首页
  而非返回 harness；redirect 参数未编码、& 截断 → 登录页放行同注册域绝对 URL（跨域仍拒绝），
  dsh 侧 redirect 参数百分号编码
- **首注册 admin 竞态**：count 后 INSERT 的顺序下并发注册可双双成为 admin →
  写事务内判定 admin 存在性（SQLite 写锁串行化），恰一个 admin
- **dsh 路由 body limit（L8）**：axum 默认 2MB 先于上游 160MB 限制拒绝大附件 → 256MB

### Tests
- 集成测试 62 → 70(+8)：journal 重放（rename/trash）、% 通配符删除隔离、并发建夹、
  超配额保留分片、zip 跳过符号链接、截断分片拒绝、dsh 凭据剥离与 redirect 编码、
  并发首注册单 admin

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
