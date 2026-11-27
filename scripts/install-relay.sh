#!/bin/sh
# AutoAgents 中继（autoagents-relay）安装脚本。
#
# 中继是一台放在公网的小程序：各台被管理的机器与运行 AutoAgents 的电脑都只向它发起出站连接，
# 所以在公司网络、家庭路由器后面也能互通。它只验令牌、转发密文，看不到任何内容。
# 一台最便宜的 VPS 就够，7000 端口要能从外面访问。
#
# 用法（Linux / macOS）：
#   curl -fsSL <本脚本> | sh                       装到 ~/.autoagents/bin，并打印怎么启动
#   curl -fsSL <本脚本> | sh -s -- --run           装好后在当前终端前台运行
#   curl -fsSL <本脚本> | sh -s -- --service       root + systemd：注册成系统服务并启动（服务器上推荐）
# 可选参数：
#   --public <wss://域名或IP:端口>   写进连接串的对外地址；不给时自动探测公网 IP
#   --port <端口>                    监听端口，默认 7000
#   --token <令牌>                   指定令牌；不给则自动生成
#
# 环境变量 AUTOAGENTS_VERSION / AUTOAGENTS_DOWNLOAD_BASE / AUTOAGENTS_NO_CHECKSUM 的含义同 install-agentd.sh

set -eu

REPO="1989tianlong/AutoAgents"
VERSION="${AUTOAGENTS_VERSION:-latest}"
BASE="${AUTOAGENTS_DOWNLOAD_BASE:-https://github.com/${REPO}/releases/download}"
# GitHub 上「最新版」的地址是 releases/latest/download/<文件>，中间没有版本号那一层
# （releases/download/latest/ 是 404）。指定了别的下载地址时，照旧按 <地址>/<版本>/ 去取
dir="${BASE}/${VERSION}"
if [ "${VERSION}" = "latest" ] && [ "${BASE}" = "https://github.com/${REPO}/releases/download" ]; then
  dir="https://github.com/${REPO}/releases/latest/download"
fi
NAME="autoagents-relay"

say() { printf '%s\n' "$*"; }
die() { printf '\n安装失败：%s\n\n' "$*" >&2; exit 1; }

fetch() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --connect-timeout 15 --retry 3 -o "${2}" "${1}" </dev/null
  else
    wget -q -O "${2}" -T 15 "${1}" </dev/null
  fi
}

# ---------- 参数 ----------
mode=install
public=""
port=7000
token=""
while [ $# -gt 0 ]; do
  case "${1}" in
    --service) mode=service ;;
    --run) mode=run ;;
    --public)
      [ $# -ge 2 ] || die "--public 需要一个地址参数"
      public="${2}"
      shift
      ;;
    --port)
      [ $# -ge 2 ] || die "--port 需要一个数字参数"
      port="${2}"
      shift
      ;;
    --token)
      [ $# -ge 2 ] || die "--token 需要一个令牌参数"
      token="${2}"
      shift
      ;;
    --help|-h)
      say "用法：curl -fsSL <本脚本> | sh -s -- [--service | --run] [--public wss://…:端口] [--port 端口] [--token 令牌]"
      exit 0
      ;;
    *) die "不认识的参数「${1}」" ;;
  esac
  shift
done
case "${port}" in
  ''|*[!0-9]*) die "端口应当是数字：${port}" ;;
esac
if [ "$(printf '%s' "${public}${token}" | wc -l | tr -d ' ')" -ne 0 ]; then
  die "参数不能包含换行符"
fi
case "${public}${token}" in
  *"$(printf '\r')"*) die "参数不能包含回车符" ;;
esac

# ---------- 认出平台 ----------
os="$(uname -s)"
arch="$(uname -m)"
case "${os}" in
  Linux) flavor=linux ;;
  Darwin) flavor=darwin ;;
  *) die "暂不支持 ${os}：中继目前提供 Linux 与 macOS 版本" ;;
esac
case "${arch}" in
  x86_64|amd64) cpu=x86_64 ;;
  aarch64|arm64) cpu=aarch64 ;;
  *) die "暂不支持 ${arch} 架构（目前提供 x86_64 与 arm64）" ;;
esac
case "${flavor}/${cpu}" in
  linux/x86_64) TARGET="x86_64-unknown-linux-musl" ;;
  linux/aarch64) TARGET="aarch64-unknown-linux-musl" ;;
  darwin/x86_64) TARGET="x86_64-apple-darwin" ;;
  darwin/aarch64) TARGET="aarch64-apple-darwin" ;;
esac
say "平台：${os} ${arch} → ${TARGET}"

need() { command -v "${1}" >/dev/null 2>&1 || die "这台机器上没有 ${1}，请先安装"; }
need tar
command -v curl >/dev/null 2>&1 || command -v wget >/dev/null 2>&1 || die "这台机器上没有 curl 或 wget"

# ---------- 下载并校验 ----------
work="$(mktemp -d 2>/dev/null || mktemp -d -t autoagents)"
trap 'rm -rf "$work"' EXIT INT TERM
asset="autoagents-node-${TARGET}.tar.gz"
url="${dir}/${asset}"
say "下载：${url}"
fetch "${url}" "${work}/${asset}" || die \
  "下载失败。如果地址打不开（404），说明 ${VERSION} 这个版本没有提供 ${TARGET} 的构建：
  https://github.com/${REPO}/releases"
if [ "${AUTOAGENTS_NO_CHECKSUM:-0}" != "1" ]; then
  fetch "${dir}/SHA256SUMS" "${work}/SHA256SUMS" || die "下载校验文件失败"
  want="$(grep " ${asset}\$" "${work}/SHA256SUMS" | awk '{print $1}')"
  [ -n "${want}" ] || die "校验文件里没有 ${asset}，这个版本不完整"
  if command -v sha256sum >/dev/null 2>&1; then
    got="$(sha256sum "${work}/${asset}" | awk '{print $1}')"
  elif command -v shasum >/dev/null 2>&1; then
    got="$(shasum -a 256 "${work}/${asset}" | awk '{print $1}')"
  else
    die "没有 sha256sum 或 shasum，无法校验。确认来源可信的话，可以加 AUTOAGENTS_NO_CHECKSUM=1 跳过"
  fi
  [ "${want}" = "${got}" ] || die "校验不对（期望 ${want}，实际 ${got}）：文件可能损坏或被篡改"
  say "校验通过：${got}"
fi

# ---------- 安装程序 ----------
if [ "${mode}" = service ]; then
  [ "${flavor}" = linux ] || die "--service 只支持 Linux；macOS 上请去掉 --service"
  [ "$(id -u)" = 0 ] || die "--service 需要 root：在命令前加 sudo"
  command -v systemctl >/dev/null 2>&1 || die "这台机器上没有 systemd，去掉 --service 再试"
  bin="/usr/local/bin/${NAME}"
  mkdir -p /usr/local/bin
  tar -xzf "${work}/${asset}" -C "${work}" "${NAME}"
  chmod 0755 "${work}/${NAME}"
  mv -f "${work}/${NAME}" "${bin}"
else
  bin="${AUTOAGENTS_INSTALL_DIR:-${HOME}/.autoagents/bin}/${NAME}"
  mkdir -p "$(dirname "${bin}")"
  tar -xzf "${work}/${asset}" -C "${work}" "${NAME}"
  chmod 0755 "${work}/${NAME}"
  mv -f "${work}/${NAME}" "${bin}"
  case ":${PATH}:" in
    *":$(dirname "${bin}"):"*) ;;
    *) say "提示：$(dirname "${bin}") 不在 PATH 里，下文里的 ${NAME} 请写成 ${bin}" ;;
  esac
fi
"${bin}" --version </dev/null

# 对外地址：没给就探测公网 IP（探测不到就让中继在启动时打占位符）
if [ -z "${public}" ]; then
  detected=""
  if command -v curl >/dev/null 2>&1; then
    detected="$(curl -fsS --connect-timeout 5 --max-time 8 https://api.ipify.org </dev/null 2>/dev/null || true)"
  fi
  case "${detected}" in
    ''|*[!0-9.]*) public="wss://<公网IP或域名>:${port}" ;;
    *) public="wss://${detected}:${port}" ;;
  esac
fi
# 转义 systemd unit 值（防止 % / $ / " / \ 注入）
sd_escape() {
  printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e 's/%/%%/g' -e 's/\$/\$\$/g'
}

case "${mode}" in
  run)
    say ""
    say "中继在前台运行，按 Ctrl+C 停止。"
    if [ -n "${token}" ]; then
      exec "${bin}" --listen "0.0.0.0:${port}" --public "${public}" --token "${token}" </dev/null
    else
      exec "${bin}" --listen "0.0.0.0:${port}" --public "${public}" </dev/null
    fi
    ;;
  service)
    # DynamicUser：不用专门建用户；StateDirectory 负责建数据目录并给它权限
    unit="/etc/systemd/system/${NAME}.service"
    pub_esc="$(sd_escape "${public}")"
    exec_line="\"${bin}\" --listen 0.0.0.0:${port} --public \"${pub_esc}\" --data-dir /var/lib/autoagents-relay"
    if [ -n "${token}" ]; then
      # 令牌单独落盘 0600，避免 0644 的 systemd unit 泄漏给本机其他用户
      mkdir -p /var/lib/autoagents-relay
      umask 077
      printf '%s' "${token}" > /var/lib/autoagents-relay/token
      chmod 0600 /var/lib/autoagents-relay/token
    fi
    cat > "${unit}" <<UNIT
[Unit]
Description=AutoAgents relay
After=network-online.target
Wants=network-online.target

[Service]
DynamicUser=yes
StateDirectory=autoagents-relay
ExecStart=${exec_line}
Restart=always
RestartSec=3
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=strict
ProtectHome=yes

[Install]
WantedBy=multi-user.target
UNIT
    chmod 0644 "${unit}"
    systemctl daemon-reload
    systemctl enable --now "${NAME}"
    say "已安装为系统服务并启动：${unit}"
    ;;
  install)
    display_args="--listen 0.0.0.0:${port} --public '${public}'"
    [ -n "${token}" ] && display_args="${display_args} --token '${token}'"
    say ""
    say "已安装到 ${bin}。"
    say ""
    say "启动（保持这个终端开着；或放进 tmux / screen）："
    say ""
    say "  ${bin} ${display_args}"
    say ""
    say "要开机自启，在 Linux 上可以注册成用户服务："
    say ""
    say "  mkdir -p ~/.config/systemd/user"
    say "  cat > ~/.config/systemd/user/${NAME}.service <<'UNIT'"
    say "  [Service]"
    say "  ExecStart=${bin} ${display_args}"
    say "  Restart=always"
    say "  [Install]"
    say "  WantedBy=default.target"
    say "  UNIT"
    say "  systemctl --user daemon-reload && systemctl --user enable --now ${NAME}"
    say ""
    say "服务器上用 root 的话，直接重新执行本脚本并加上 --service，一步到位。"
    say ""
    say "现在启动它，把打印的「中继连接串」粘贴到 AutoAgents 的「机器 → 中继设置」。"
    exit 0
    ;;
esac

# 系统服务：从日志里把连接串找出来
say "正在读取连接串…"
found=""
i=0
while [ ${i} -lt 50 ]; do
  found="$(journalctl -u "${NAME}" --no-pager -n 80 2>/dev/null | grep -m1 'autoagents://relay?' || true)"
  [ -n "${found}" ] && break
  i=$((i + 1))
  sleep 0.2 2>/dev/null || sleep 1
done
say ""
if [ -n "${found}" ]; then
  say "把下面这行「中继连接串」粘贴到 AutoAgents 的「机器 → 中继设置」："
  say ""
  say "  ${found}"
else
  say "服务已启动，但还没从日志里读到连接串。稍后执行下面这条命令查看："
  say ""
  say "  journalctl -u ${NAME} -n 40 --no-pager"
fi
say ""
say "确认 ${port} 端口能从外面访问（云主机的安全组、ufw / firewalld）："
say "  ufw allow ${port}/tcp"
say ""
say "常用命令：systemctl status ${NAME}   journalctl -u ${NAME} -f"
