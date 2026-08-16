# pinas 代码审查报告

| 项 | 值 |
|---|---|
| 审计对象 | Antifield Cloud (Pi-NAS) **v1.11.0**（commit `6a16a32`） |
| 审计日期 | 2026-08-16 |
| 审计范围 | 全部 Rust 源码（~10.4k 行）、Askama 模板、`assets/app.js`、`static/sw.js`、测试、CI/部署脚本、生产端口行为 |
| 审计方法 | 人工通读核心安全模块 + 三路并行深审（文件操作/分享·DAV·dsh/DB·前端·测试·CI）+ **隔离环境实证探针** + 生产端口探测 |
| 结论速览 | **发现 2 个 P0（已实证复现）、10 个 P1、15 个 P2**。主干安全设计高于同类自托管项目，但文件写路径存在一个架构级校验遗漏，**P0 必须立即修复** |

---

## 一、总体评价

**做得好的（已逐项核验，非客套）：**

- **路径安全**：`fsutil::Sandbox` 用 `openat2(RESOLVE_BENEATH | NO_MAGICLINKS)` + `*at` 系统调用族把符号链接 TOCTOU 归零；18 个调用点保留 `safe_join_sandbox` 字符串级纵深防御
- **认证会话**：Argon2id（m=19MiB/t=3）+ 哑哈希等时校验（抹平用户枚举时序）、会话双闸（绝对 7 天 + 空闲 24h）、改密/重置全会话失效 + DAV 缓存同步失效、Cookie `HttpOnly; SameSite=Strict; Secure`
- **XSS 防线**：Askama 自动转义 + CSP 无 `unsafe-inline`（仅 2 个哈希放行的预涂脚本）+ html/svg/xml 强制 `attachment` + `hx-vals` 一律 `|json`
- **数据一致性**：配额全事务化（含写锁抢占的全量重算）、`fs_journal` 意图日志 + 启动幂等重放、FTS 触发器与 files 写路径全同步、迁移单事务 + 幂等
- **注入面**：全部 SQL 走绑定参数；排序/列名用白名单；FTS MATCH 双引号转义
- **dsh 反代**：Host/Cookie/Authorization 剥离重注入、WS 端点白名单 + 32 并发信号量、admin 门禁

**核心问题**：多用户共享同一 `uploads/` 根沙箱，而内核级 `RESOLVE_BENEATH` **允许根内 `..` 向上再向下解析**。把"用户间隔离"托付给内核沙箱时，`rename/move/delete/merge` 四个写路径的 `parent` 参数恰好没有做字符串级 `..` 拦截（其余 18 个调用点都做了）。这是一个"每层防御单看都正确、组合起来出现缺口"的架构级疏漏。

---

## 二、P0 — 必须立即修复（已在隔离测试环境实证复现）

### P0-1 跨用户文件操作：写路径 `parent` 未拦截 `..` 【已实证】

**位置**：
- `src/handlers/file_ops/core.rs:336-454`（`rename_core`/`move_core`）、`core.rs:458-515`（`delete_to_trash`）
- 入口：`file_ops/api.rs:238-307`（rename/move）、`api.rs:441-507`（delete/delete_batch）、`file_ops/fragments.rs:373-410`（HTMX 同名入口）、`upload.rs:490-512`（merge 的 `parent_path`）

**问题链**：
1. 入口 `parent = user_dir_path(payload.current_path)`（`utils.rs:336`）只 trim，**不拒绝 `..`**
2. 核心函数仅 `validate_name(name)`，`parent` 原样进入 `user_file_path()` → `"attacker/../victim/secret/doc.txt"`
3. `Sandbox::new(UPLOADS_DIR)` 以整个 `uploads/` 为根，`openat2 BENEATH` 允许根内 `..` 解析（`core.rs:466` 注释自述此行为）
4. `sb.rename()` 落到 `uploads/victim/secret/doc.txt` —— **跨用户越权成立**
5. 旁证：`move_batch`（`api.rs:335`）同场景正确地用了 `safe_join_sandbox`，说明这四个写路径是遗漏而非设计

**实证记录**（隔离测试环境，内存 DB + 临时目录，双用户）：

```
move   status=200 OK  stolen_to_attacker=true  original_gone=true
       STOLEN CONTENT: "VICTIM-DATA"           ← 跨用户窃取成功
delete status=200 OK  victim_file_gone=true    landed_in_trash=true
                                                  ← 跨用户删除成功
```

**影响**：任意登录用户可**读取（移入自己目录后下载）、移动、删除**服务器上任意其他用户的文件；`delete_to_trash` 还会把受害者文件移入共享回收站并因 DB 行 `username` 不匹配残留 → reconcile 判定"磁盘缺失"清库，造成受害者数据**永久丢失**。merge 路径可**向他人目录植入任意内容**。

**修复方案（阶段 0 止血 + 阶段 1 根治，见第五节）**

### P0-2 rename/move 源名未校验，可移走整棵用户子树

**位置**：`src/handlers/file_ops/core.rs:336-346`

**问题**：`rename_core` 只 `validate_name(new_name)`，不校验源名 `old_name`。`old_name=".."` 时 `old_rel="alice/dir/.."` → 解析为 `alice`（用户根目录），`sb.rename` 可把**整棵用户子树**移走；DB 无 `name='..'` 行不更新 → 物理与 DB 彻底错位 → reconcile 大规模误判孤儿清库。与 P0-1 组合可放大为跨用户破坏。

**修复**：rename 源名、move 源 `name` 一律过 `validate_name`。

---

## 三、P1 — 高优先级（正确性 / 安全控制失效）

| # | 位置 | 问题 | 影响 | 修复 |
|---|---|---|---|---|
| P1-1 | `middleware/csp.rs:32-33` vs `templates/partials/theme_head.html:2-8` | **CSP 第二哈希失配**（实测：theme_head 脚本哈希为 `V1ONiGmI3S/fo...=`，CSP 放行的却是 `fUQGwXEX59qh...=`） | `/login`、`/change-password`、`/s/*` 的暗色预涂脚本**正被 CSP 拦截**（白闪）；且漂移无从察觉（现有测试只断言 `sha256-` 前缀存在） | 更新哈希为 `V1ONiGmI3S/fo/iVTzDdp1MvwZTNDqZJWdZu1ok6Ees=`；把"脚本→哈希"一致性校验并入 `check-versions.sh` |
| P1-2 | `share.rs:586-611` `list_directory_files` | 目录分享 JSON 的 `path` 直接拼 `dir_rel`（相对 `uploads/` 根，**含拥有者用户名**） | 匿名访客枚举内网 username + 完整目录拓扑（社工/枚举情报） | `path` 裁剪为相对 share_base 的子路径 |
| P1-3 | `admin.rs:300-303` | `VACUUM INTO` 全库拷贝在 async worker 同步执行，未 `spawn_blocking`，无并发互斥 | 大库备份冻结全站数秒~数十秒 | 移入 `spawn_blocking` + `Semaphore(1)` |
| P1-4 | `upload.rs:191-204, 317-325` | 分片 5GB 待合并上限的"预检"与"累加"非原子（TOCTOU），并发可全部通过 | 临时分片目录远超 5GB，**系统盘 DoS** | 预检+累加放进同一 SQLite 写事务 |
| P1-5 | `share.rs:24-52` | 分享失败锁定按 `code` 键（跨 IP 共享），任意 IP 错 5 次即锁定合法分享 15 分钟 | 得知 code 即可 DoS 合法访问者 | 改按 `IP+code` 组合键计数锁定 |
| P1-6 | `dav/ops.rs:400-428, 484-500` | 无 `Content-Length` 的 chunked PUT：先落盘最多 5GiB 到 `uploads/tmp`，事后才配额校验 | 单用户写盘放大（有 24h 清扫兜底） | 流式写入时增量校验配额/PENDING_CHUNKS_CAP |
| P1-7 | `journal.rs:229-235` + `dav/ops.rs:714-726` | `recover_dav_disp` 的 `parent` 来自用户可控的 DAV `Destination`（`d_parent` 未校验 `..`），崩溃恢复期把位移文件写回 uploads 根内**任意子树** | 崩溃窗口触发的跨用户写入通道 | 恢复时 `safe_join_sandbox` 校验 + 断言首段 == username；DAV 侧逐段拒绝 `..` |
| P1-8 | `trash.rs:290-320, 244-286` | 清空/永久删除逐条"物理删 + 删 DB 行"，物理失败被 `let _ =` 吞错、**DB 行照删**，无事务 | 磁盘孤儿持续占盘；崩溃半清空态 | 事务化；物理失败即中止回滚 |
| P1-9 | `core/auth.rs:34-36` + `dsh.rs build_dsh_router` | dsh 路由复用 `auth_middleware`，其中 `/s/` 前缀**豁免**在 dsh 域同样生效：未认证请求 `/s/*` 被放行 → `UserSession` 缺失 → **生产实测 500**；`/api/media/*?mt=` 媒体令牌路径同理可达 dsh 上游 | 当前偶然 fail-closed（500），但是**潜在认证绕过**：任何让 session 变为可选的重构都会打开缺口 | `auth_middleware` 的 `/s/` 豁免改由主路由声明（如按路由集配置），或 dsh 路由用专用门禁中间件 |
| P1-10 | `file_ops/core.rs:490-508` | `delete_to_trash` 的 `INSERT INTO trash` + `db_delete_file_rows`（父+子两条 DELETE）未包单事务（有 fs_journal 兜底） | 崩溃窗口多一个恢复分支 | 包入单一事务 |

---

## 四、P2 — 改进项

### 安全加固
| # | 位置 | 问题 | 建议 |
|---|---|---|---|
| P2-1 | `core/auth.rs:96-105` | 媒体令牌前缀比对用**原始百分号编码** `uri.path()`，handler 用解码后路径（规范化歧义；因 handler 另有 `safe_join_sandbox` 兜底，最坏同用户内越前缀，不跨用户） | auth 层按解码后路径逐段比较 |
| P2-2 | `rate_limit.rs:16-42` | 容量清理用**当前端点 window** retain；注册（1h）键堆满 10k 后登录（60s）清不动 → 跨端点 DoS（需 10k 真实 IP） | 值内记录各自 window，按过期时间全局清理 |
| P2-3 | `handlers/auth.rs:129-154` `extract_ip` | 对**任意回环对端**信任 `CF-Connecting-IP`/`X-Forwarded-For`（当前纯 cloudflared 接入安全；换 nginx/caddy 透传即可伪造） | 收敛为仅信任 cloudflared 本地端口对端 |
| P2-4 | `handlers/auth.rs:275-281` | 登录响应 JSON 回显明文会话 token（dsh-plugin Bearer 依赖，已接受）；页面 XSS 时 token 与 cookie 同源可读 | 浏览器场景（无 Bearer 标志）不回显 |
| P2-5 | `handlers/auth.rs:439-448` | `change_password` 只查绝对过期、**不查空闲超时**（与中间件双闸规则不一致），且 DB 错误 `unwrap_or(None)` 吞成 401 | 会话查询复用中间件同一规则 |
| P2-6 | `admin.rs:357-371` | `download_backup` filename 仅拦 `/` 与 `..`，未防符号链接（当前无写入面，仅 admin） | 走沙箱或 canonicalize 校验 |
| P2-7 | `middleware/request_id.rs:19-24` | 入站 `X-Request-Id` 不限长，进每行日志 | 截断至 ≤128 字符 |
| P2-8 | `system.rs:113-115` | `/health` 匿名暴露版本号 | 可接受（标准实践），或内网限定 |
| P2-9 | `templates/pages/share.html:32` | 分享密码回显进 URL 查询串（地址栏/历史/日志） | 改一次性短时效令牌（仿 media_tokens） |
| P2-10 | `core/crypto.rs:53-61` | `generate_random_password` 用 `% 70` 取模（256%70≠0，前 46 字符概率 4/256 vs 3/256）；24 字符熵仍 ~147bit，实际可忽略 | 拒绝采样消偏差（可选） |

### 一致性与残留（v1.11 清理遗漏）
| # | 位置 | 问题 | 建议 |
|---|---|---|---|
| P2-11 | `config.rs:161-162` | 敏感键回显过滤仍列 `PINAS_DEEPSEEK_API_KEY`/`PINAS_MASTER_KEY`（AI 已移除） | 删除两个键名 |
| P2-12 | `main.rs:2` | 头注释仍写"自托管 NAS 网盘 **+ AI 助手**" | 更新注释 |
| P2-13 | `templates/components/admin_users.html:21-22` | `hx-vals`/`hx-confirm` 未用 `\|json`（用户名白名单兜底，当前不可利用） | 补 `\|json` |

### 健壮性与工程
| # | 位置 | 问题 | 建议 |
|---|---|---|---|
| P2-14 | `trash.rs:359-377` vs `restore_trash:78` | 后台过期清理与用户还原同一 uuid 无互斥（低概率误删还原中文件） | per-uuid/per-user 互斥 |
| P2-15 | `tasks/cleanup.rs:316-379` | 按 mtime 清扫分片目录：暂停超 `PINAS_TEMP_CLEANUP_HOURS` 的断点续传可能被清 → merge 中断 | 豁免 DB 中存活的 `upload_chunks` 记录 |
| P2-16 | `media.rs:412-423`、`file_ops/core.rs:48-115` | async 内同步阻塞调用（`Path::exists`）；reconcile 每行新建 Sandbox（重复建 root fd） | 统一 `spawn_blocking`；reconcile 共享一个 Sandbox |
| P2-17 | `dav/ops.rs:47,74,142,...` | 多处 `unwrap_or_default()`/`unwrap_or(false)` 吞 IO/DB 错误且无日志 | 真实异常补 `tracing::warn!` |
| P2-18 | `scripts/deploy.sh:41-85` | start 失败不自动回滚；本地构建无 `-D warnings`（与 CI 不一致） | 失败自动回滚 `.bak.pre-*`；构建对齐 CI |
| P2-19 | `scripts/check-versions.sh` | 不校验 `app.js:57` 懒加载库（`marked.min.js?v=1`、`purify.min.js?v=1`）与 SW 预缓存一致性；CSP 哈希无自动校验（见 P1-1） | 扩扫描面 + 哈希校验 |
| P2-20 | `.github/workflows/ci.yml:43` | `embiid/cargo-deny-action@v2` 可变标签 | pin 到提交 SHA |

---

## 五、修复路线图（改进方案）

### 阶段 0 — P0 止血（最小修复，建议立即，~0.5 天）

**方案 A（字符串层，最小 diff）**：
1. 新增 `validate_rel_path(path) -> AppResult<()>`：逐段拒绝 `..`、空段、`.`、绝对路径（复用 `validate_name` 的字符白名单逻辑，按 `/` 分段）
2. 在四个写路径入口统一调用：`rename_core`/`move_core`（parent、src_parent、dst_parent）、`delete_to_trash`（parent_path）、`merge_chunks`（parent_path）
3. `rename_core`/`move_core` 源名（`old_name`/`name`）补 `validate_name`（修 P0-2）
4. 顺路修 P1-7：`recover_dav_disp` 同一函数校验 + 首段断言 == username；DAV `d_parent` 逐段拒绝 `..`
5. **回归测试**：把本次实证探针反转为断言拒绝（`../victim` parent → 4xx，磁盘文件不动；`..` 源名 → 4xx），常驻 CI

**方案 B（架构根治，推荐随后实施，~2-3 天）**：
- 用户文件操作改用**每用户子树沙箱** `Sandbox::new(uploads/{username})`，rel 不再拼 username —— 用户间隔离上收到内核层，字符串校验退化为纯纵深防御
- 回收站随之改为 `uploads/{username}/.trash/`（天然按用户隔离；解决"跨用户删除进共享回收站"的第二重问题；常量注释已解释为何须在 tmp 外，用户子树内同样满足）
- 代价：`share.rs`/`media.rs` 等按 `uploads/` 根 + `username/rel` 拼路径的调用点需统一改造；回归面大，需全量测试

> 建议：A 立即上（阻断漏洞），B 作为 v1.12.0 主线（消除同类问题再生土壤）。

### 阶段 1 — P1 批量（~2 天）

| 顺序 | 项 | 要点 |
|---|---|---|
| 1 | P1-1 CSP 哈希 | 更新为实测值 + `check-versions.sh` 加"脚本→sha256"自动校验（防再漂移） |
| 2 | P1-2 分享路径泄露 | `path` 裁剪为相对 share_base |
| 3 | P1-9 dsh `/s/` 豁免 | `/s/` 豁免从 `auth_middleware` 下沉为主路由专属（如参数化豁免前缀，dsh 路由传空） |
| 4 | P1-4 分片上限原子化 | 预检+累加并入单写事务 |
| 5 | P1-3 VACUUM | `spawn_blocking` + 信号量互斥 |
| 6 | P1-5 分享锁定 | 改 `IP+code` 组合键 |
| 7 | P1-6/P1-8/P1-10 | DAV 流式增量配额；回收站清空事务化+失败中止；trash 登记+files 删除单事务 |

### 阶段 2 — P2 择需（随版本顺带）
- 清理残留（P2-11/12/13，随阶段 0 顺手做，成本 5 分钟）
- 模板/工程类（P2-13/18/19/20）随下次构建流程调整
- 安全加固类（P2-1/2/3/5）按部署形态评估

---

## 六、测试覆盖缺口（对照源码功能）

| 缺口 | 风险 |
|---|---|
| **写路径 `..` 拒绝无回归测试**（本次 P0 的直接成因） | 阶段 0 必须补齐：rename/move/delete/merge 的 `..` parent、源名 `..`、绝对路径、空段 |
| `tasks/cleanup.rs`：过期回收站清理、会话/审计/WAL checkpoint/分片行清理均无运行期测试 | 后台误删无防线（P2-14/15 的测试基础） |
| `media.rs save_file_content_handler`（编辑器保存：配额差值预留/失败自愈/回滚） | 配额漂移路径未覆盖 |
| `admin.rs`：`set_user_quota`/`reset_password`/`download_backup` | 管理面回归靠手测 |
| `reconcile_files_on_disk` 批量孤儿清理 | P0 修复后 DB 错位场景的行为断言缺失 |
| CSP 哈希真实性（现有测试只查前缀） | 随 P1-1 修复一并补 |

---

## 七、已核验无问题项（审计留痕）

- Basic 认证时序（哑哈希等时）、DAV 缓存指纹无碰撞绕过、会话固定、改密后全失效
- 分享三处密码校验均在下载前；失败计数 Mutex 串行无并发丢失；下载强制 attachment
- dsh 门禁：WS/HTTP 均过 admin 检查；`is_privileged_api` 路径变体只会降级不会提权
- SQL 注入面：全绑定参数；`QueryBuilder.push_bind`；排序白名单；FTS 转义
- 模板 XSS：无 `|safe` 滥用；markdown 原文 serde_json + `<` 转义；`javascript:` URL 有测试
- 迁移幂等/单事务/版本拒绝回滚；配额事务无 TOCTOU；FTS 触发器全同步
- 错误响应不泄露内部路径（`internal_log` 仅记日志，客户端通用文案）
- Range 解析边界/后缀/416 处理正确；merge O_EXCL + 完整性校验；MIME 双闸（首片 512B + merge 全量复核）

---

*报告完。下一步由项目 owner 决策：是否按第五节路线图实施（建议阶段 0 立即执行）。*
