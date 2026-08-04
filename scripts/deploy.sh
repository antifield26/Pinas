#!/usr/bin/env bash
# ====== Antifield Cloud 部署脚本 (v1.5.1) ======
# 按 git HEAD 构建 → 部署到 ~/pinas → 记录版本 → 重启服务 → 验证
# 用法: scripts/deploy.sh [--skip-build] [--tag v1.5.1]
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
DEPLOY_DIR="${PINAS_DEPLOY_DIR:-$HOME/pinas}"
SERVICE="${PINAS_SERVICE:-antifield-cloud.service}"
COMMIT_SHA="$(git -C "$PROJECT_DIR" rev-parse --short HEAD)"
COMMIT_DATE="$(git -C "$PROJECT_DIR" log -1 --format=%cd --date=short)"
VERSION="$(grep -m1 '^version' "$PROJECT_DIR/Cargo.toml" | cut -d'"' -f2)"

echo "==> 构建版本: $VERSION (commit $COMMIT_SHA, $COMMIT_DATE)"

# 1. 构建前检查:工作树是否干净(部署必须可复现)
if [ -n "$(git -C "$PROJECT_DIR" status --porcelain)" ]; then
  echo "!! 警告: git 工作树有未提交改动,部署产物将不可复现"
  echo "   $(git -C "$PROJECT_DIR" status --porcelain | head -5)"
  echo "   5 秒后继续(或 Ctrl-C 中止)..."
  sleep 5
fi

# 2. 构建
if [ "${1:-}" != "--skip-build" ]; then
  echo "==> cargo build --release ..."
  (cd "$PROJECT_DIR" && cargo build --release)
fi

# 3. 停服务并部署
echo "==> 部署到 $DEPLOY_DIR ..."
sudo systemctl stop "$SERVICE"
cp "$PROJECT_DIR/target/release/pi_nas" "$DEPLOY_DIR/pi_nas"
chmod +x "$DEPLOY_DIR/pi_nas"
# 模板/静态资源随二进制,以下仅在 PWA/CSS 更新时必拷
if [ -f "$PROJECT_DIR/static/sw.js" ]; then
  cp "$PROJECT_DIR/static/sw.js" "$DEPLOY_DIR/static/sw.js"
fi
if [ -f "$PROJECT_DIR/assets/css/tailwind.min.css" ]; then
  cp "$PROJECT_DIR/assets/css/tailwind.min.css" "$DEPLOY_DIR/assets/css/tailwind.min.css"
fi
if [ -f "$PROJECT_DIR/assets/manifest.json" ]; then
  cp "$PROJECT_DIR/assets/manifest.json" "$DEPLOY_DIR/assets/manifest.json"
fi

# 4. 记录部署版本(可复现溯源)
echo "$VERSION commit=$COMMIT_SHA date=$COMMIT_DATE deployed=$(date '+%Y-%m-%d %H:%M:%S')" > "$DEPLOY_DIR/VERSION"

# 5. 启动并验证
sudo systemctl start "$SERVICE"
sleep 2
HEALTH="$(curl -s -m 5 http://localhost:3000/health || true)"
echo "==> /health: $HEALTH"
case "$HEALTH" in
  *"$VERSION"*) echo "==> ✅ 部署成功 ($VERSION)";;
  *) echo "==> ❌ 健康检查异常,请查看: journalctl -u $SERVICE -n 50"; exit 1;;
esac
