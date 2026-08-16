#!/usr/bin/env bash
# ====== 版本对齐检查:模板 ?v= 与 sw.js PRE_CACHE_URLS 必须严格一致 ======
# 历史反复出现的 bug:CSS/JS 版本号更新时漏改其中一处,导致 SW 预缓存 miss。
# 用法: scripts/check-versions.sh
set -uo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
FAIL=0

# 提取模板中所有 /assets/xxx?v=N 引用
# 注：字符类必须含 /（嵌套路径如 /assets/css/tailwind.min.css）与 json（manifest）——
# 历史正则漏掉二者，CSS 版本校验形同虚设（login/change_password/share 曾漂移 v16 未被拦截）
echo "==> 模板 ?v= 引用:"
TEMPLATE_VERSIONS="$(grep -rhoE '/assets/[a-zA-Z0-9._/-]+\.(css|js|json)\?v=[0-9]+' "$PROJECT_DIR/templates" | sort -u)"
echo "$TEMPLATE_VERSIONS"

echo "==> sw.js 预缓存:"
SW_VERSIONS="$(grep -oE '/assets/[a-zA-Z0-9._/-]+\.(css|js|json)\?v=[0-9]+' "$PROJECT_DIR/static/sw.js" | sort -u)"
echo "$SW_VERSIONS"

# 每项模板引用必须在 SW 预缓存中存在,且版本号一致
for entry in $TEMPLATE_VERSIONS; do
  asset="${entry%%\?*}"
  ver="${entry##*\?v=}"
  if ! echo "$SW_VERSIONS" | grep -q "$asset"; then
    echo "!! ❌ $asset 不在 sw.js 预缓存中"; FAIL=1
  elif ! echo "$SW_VERSIONS" | grep -q "${asset}?v=${ver}"; then
    echo "!! ❌ $asset 模板版本 v$ver 与 SW 预缓存不一致"; FAIL=1
  fi
done

# SW 注册版本号一致性（app.js 的 ?v= 与 sw.js 头注释版本号；主脚本已外置到 assets/app.js）
SW_REG="$(grep -oE "sw\.js\?v=[0-9]+" "$PROJECT_DIR/assets/app.js" | head -1)"
SW_HEADER="$(head -3 "$PROJECT_DIR/static/sw.js" | grep -oE 'v[0-9]+' | head -1 | tr -d 'v')"
echo "==> SW 注册: $SW_REG ; SW 文件头版本: v$SW_HEADER"
if [ "$SW_REG" != "sw.js?v=${SW_HEADER}" ]; then
  echo "!! ❌ SW 注册版本与文件头不一致: $SW_REG vs v$SW_HEADER"; FAIL=1
fi

# ====== CSP 内联脚本 → sha256 一致性校验 ======
# 仅 templates/base.html 与 templates/partials/theme_head.html 的两处 {theme} 预涂脚本内联，
# 其余业务脚本一律外置；模板无 'unsafe-inline'，故每个内联脚本必须被 csp.rs 声明哈希放行。
# 哈希取脚本 innerText 逐字节（含缩进/换行），与浏览器 CSP 计算方式一致（不含脚本标签本身）。
echo "==> CSP 内联脚本 sha256 校验:"
# python3 计算: 提取两文件内联 <script> innerText → sha256 base64;读取 csp.rs 声明集合;
# 断言两集合完全一致（顺序无关),任一侧多/少皆报错并非零退出。
python3 - "$PROJECT_DIR/src/middleware/csp.rs" "$PROJECT_DIR/templates/base.html" "$PROJECT_DIR/templates/partials/theme_head.html" <<'PYEOF'
import re, hashlib, base64, sys
csp_file, base_file, theme_file = sys.argv[1], sys.argv[2], sys.argv[3]

declared = set(re.findall(r'sha256-[A-Za-z0-9+/=]+', open(csp_file, encoding='utf-8').read()))

computed = set()
for f in (base_file, theme_file):
    content = open(f, encoding='utf-8').read()
    for m in re.finditer(r'<script>(.*?)</script>', content, re.S):
        # CSP 哈希取脚本 innerText 逐字节(含缩进/换行), 与浏览器一致(不含 <script> 标签)
        b64 = base64.b64encode(hashlib.sha256(m.group(1).encode('utf-8')).digest()).decode()
        computed.add('sha256-' + b64)

print('csp.rs 声明:  ' + ' '.join(sorted(declared)))
print('内联脚本哈希:  ' + ' '.join(sorted(computed)))

if declared == computed:
    print('   ✅ CSP 内联脚本哈希集合与 csp.rs 声明一致')
else:
    missing = computed - declared   # 未在 csp.rs 声明的内联脚本
    extra = declared - computed     # csp.rs 声明的哈希无对应脚本(过期/漂移)
    if missing:
        print('!! ❌ 存在未在 CSP 声明的内联脚本(新增内联脚本需同步 csp.rs): ' + ' '.join(sorted(missing)))
    if extra:
        print('!! ❌ csp.rs 声明哈希无对应现有内联脚本(过期或模板已改动): ' + ' '.join(sorted(extra)))
    sys.exit(1)
PYEOF
[ $? -ne 0 ] && FAIL=1

# ====== 懒加载库 URL 一致性校验 ======
# app.js 首次渲染 markdown 时动态注入 marked/purify（懒加载，全站不预载），
# 其 ?v= 必须与 sw.js PRE_CACHE 列表严格一致，否则预缓存 miss 且离线缺失。
echo "==> app.js 懒加载库 URL:"
LAZY_URLS="$(grep -oE '/assets/(marked|purify)\.min\.js\?v=[0-9]+' "$PROJECT_DIR/assets/app.js" | sort -u)"
echo "$LAZY_URLS"
for entry in $LAZY_URLS; do
  if ! echo "$SW_VERSIONS" | grep -q "${entry}"; then
    echo "!! ❌ $entry 不在 sw.js 预缓存中"; FAIL=1
  fi
done

if [ "$FAIL" -eq 0 ]; then
  echo "==> ✅ 版本对齐检查通过"
else
  echo "==> ❌ 版本对齐检查失败,请统一上述版本号"
  exit 1
fi
