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

# SW 注册版本号一致性（base.html 的 ?v= 与 sw.js 头注释版本号）
SW_REG="$(grep -oE "sw\.js\?v=[0-9]+" "$PROJECT_DIR/templates/base.html" | head -1)"
SW_HEADER="$(head -3 "$PROJECT_DIR/static/sw.js" | grep -oE 'v[0-9]+' | head -1 | tr -d 'v')"
echo "==> SW 注册: $SW_REG ; SW 文件头版本: v$SW_HEADER"
if [ "$SW_REG" != "sw.js?v=${SW_HEADER}" ]; then
  echo "!! ❌ SW 注册版本与文件头不一致: $SW_REG vs v$SW_HEADER"; FAIL=1
fi

if [ "$FAIL" -eq 0 ]; then
  echo "==> ✅ 版本对齐检查通过"
else
  echo "==> ❌ 版本对齐检查失败,请统一上述版本号"
  exit 1
fi
