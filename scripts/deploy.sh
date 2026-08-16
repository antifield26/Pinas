#!/usr/bin/env bash
# ====== Antifield Cloud 部署脚本 (v1.9.0) ======
# 按 git HEAD 构建 → 部署到 ~/pinas → 记录版本 → 重启服务 → 验证
# 用法: scripts/deploy.sh [--skip-build] [--allow-dirty]
# 部署纪律：
#   - 工作树必须干净（脏树部署产物不可复现）——历史事故：未提交的 dsh 反代以旧版本号上线，
#     VERSION 文件与实际二进制对不上；默认拒绝，确需临时部署显式 --allow-dirty
#   - 覆盖前自动备份现网二进制（pi_nas.bak.pre-{版本}），失败可回滚
#   - systemd unit 的 Description 同步当前版本（/etc/systemd/system + 部署目录副本）
#   v1.9.0：构建对齐 CI 加 RUSTFLAGS="-D warnings"；启动失败/健康探测失败自动回滚备份二进制
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
DEPLOY_DIR="${PINAS_DEPLOY_DIR:-$HOME/pinas}"
SERVICE="${PINAS_SERVICE:-antifield-cloud.service}"
COMMIT_SHA="$(git -C "$PROJECT_DIR" rev-parse --short HEAD)"
COMMIT_DATE="$(git -C "$PROJECT_DIR" log -1 --format=%cd --date=short)"
VERSION="$(grep -m1 '^version' "$PROJECT_DIR/Cargo.toml" | cut -d'"' -f2)"
ALLOW_DIRTY="${2:-}${1:-}"

echo "==> 构建版本: $VERSION (commit $COMMIT_SHA, $COMMIT_DATE)"

# 1. 构建前检查：工作树必须干净（部署必须可复现）
if [ -n "$(git -C "$PROJECT_DIR" status --porcelain)" ]; then
  if [ "$ALLOW_DIRTY" != "--allow-dirty" ] && [ "${1:-}" != "--allow-dirty" ]; then
    echo "!! ❌ git 工作树有未提交改动，部署产物将不可复现："
    echo "   $(git -C "$PROJECT_DIR" status --porcelain | head -10)"
    echo "   请先提交；确需临时部署显式加 --allow-dirty"
    exit 1
  fi
  echo "!! 警告: --allow-dirty 指定的脏树部署，产物不可复现"
fi

# 2. 构建（与 CI 对齐：-D warnings 把警告当错误，防带警告产物上线）
if [ "${1:-}" != "--skip-build" ]; then
  echo "==> cargo build --release (RUSTFLAGS=\"-D warnings\") ..."
  (cd "$PROJECT_DIR" && RUSTFLAGS="-D warnings" cargo build --release)
fi

# 3. 停服务并部署（覆盖前备份现网二进制）
echo "==> 部署到 $DEPLOY_DIR ..."
sudo systemctl stop "$SERVICE"
if [ -f "$DEPLOY_DIR/pi_nas" ]; then
  cp "$DEPLOY_DIR/pi_nas" "$DEPLOY_DIR/pi_nas.bak.pre-${VERSION}"
  echo "==> 已备份现网二进制: pi_nas.bak.pre-${VERSION}"
fi
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
# JS 库（htmx/alpine/marked/purify）+ 主脚本 app.js（非 .min.js 命名，需显式拷贝）
# 同样必须拷贝——全新部署缺它们页面无交互
for f in "$PROJECT_DIR"/assets/*.min.js; do
  [ -f "$f" ] && cp "$f" "$DEPLOY_DIR/assets/"
done
if [ -f "$PROJECT_DIR/assets/app.js" ]; then
  cp "$PROJECT_DIR/assets/app.js" "$DEPLOY_DIR/assets/app.js"
fi

# 3.5 systemd unit Description 同步当前版本（部署目录副本 + /etc/systemd/system）
if [ -f "$DEPLOY_DIR/antifield-cloud.service" ]; then
  sed -i "s/^Description=.*/Description=Antifield Cloud v$VERSION/" "$DEPLOY_DIR/antifield-cloud.service"
  sudo cp "$DEPLOY_DIR/antifield-cloud.service" "/etc/systemd/system/$SERVICE"
  sudo systemctl daemon-reload
fi

# 4. 记录部署版本(可复现溯源)
echo "$VERSION commit=$COMMIT_SHA date=$COMMIT_DATE deployed=$(date '+%Y-%m-%d %H:%M:%S')" > "$DEPLOY_DIR/VERSION"

# 5. 启动并验证（失败自动回滚到部署前备份）
rollback() {
  local reason="$1"
  local bak="$DEPLOY_DIR/pi_nas.bak.pre-${VERSION}"
  echo "==> 🔙 部署失败($reason)，开始回滚..."
  if [ ! -f "$bak" ]; then
    echo "!! ❌ 无回滚备份 $bak，无法回滚" >&2
    return 1
  fi
  sudo cp "$bak" "$DEPLOY_DIR/pi_nas"
  sudo chmod +x "$DEPLOY_DIR/pi_nas"
  echo "==> 已回滚二进制: $bak → $DEPLOY_DIR/pi_nas"
  sudo systemctl restart "$SERVICE"
  local rhealth
  rhealth="$(curl -s -m 5 http://localhost:3000/health || true)"
  echo "==> 回滚后 /health: $rhealth"
  # 回滚后进程存活且健康即视为回滚成功（不再校验旧版本号串）
  if sudo systemctl is-active --quiet "$SERVICE" && echo "$rhealth" | grep -q .; then
    echo "==> ✅ 已回滚并恢复服务 (systemd active)"
    return 0
  fi
  echo "!! ❌ 回滚后服务仍异常: journalctl -u $SERVICE -n 50" >&2
  return 1
}

echo "==> 启动并验证..."
# start 失败（非零）→ 回滚；成功后再探 /health，3 秒内任一次探测不带当前版本号 → 回滚
if ! sudo systemctl start "$SERVICE"; then
  rollback "systemctl start 返回非零" || { echo "!! ❌ 回滚失败，退出"; exit 1; }
  exit 0
fi

HEALTH=""
for i in 1 2 3; do
  sleep 1
  HEALTH="$(curl -s -m 5 http://localhost:3000/health || true)"
  if echo "$HEALTH" | grep -q "$VERSION"; then break; fi
done
echo "==> /health: $HEALTH"
case "$HEALTH" in
  *"$VERSION"*) echo "==> ✅ 部署成功 ($VERSION)";;
  *) echo "==> /health 3 秒内未带版本号 $VERSION，触发回滚";
     rollback "health 探测失败" || { echo "!! ❌ 回滚失败，退出"; exit 1; };
     exit 0;;
esac
