#!/bin/sh
# AutoAgents agentd 安装脚本 —— 把这台机器变成 AutoAgents 可以管理的节点。
#
# 通常不用手动运行。AutoAgents「机器 → 接入新机器」生成的命令就是：
#   curl -fsSL <本脚本的地址> | sh -s -- install --name <名字> --relay <wss://…> --token <令牌> \
#       --pair <配对码> [--fp <证书指纹>] [--allow <目录>]…
#
# 它做四件事：
#   1. 认出操作系统与 CPU 架构
#   2. 从 GitHub Releases 下载对应的 agentd，并核对 SHA-256
#   3. 装到 ~/.autoagents/bin/
#   4. 把后面的参数原样交给 `agentd install`：保存配置、注册成开机自启的服务并启动
#
# 可以用环境变量覆盖：
#   AUTOAGENTS_VERSION=v0.2.0        指定版本（默认 latest）
#   AUTOAGENTS_DOWNLOAD_BASE=<地址>  下载地址的前缀（内网镜像；默认是 GitHub Releases）
#   AUTOAGENTS_INSTALL_DIR=<目录>    安装位置（默认 ~/.autoagents/bin）
#
# 只想升级 agentd、不动配置：curl -fsSL <本脚本> | sh -s -- upgrade

set -eu

REPO="1989tianlong/AutoAgents"
VERSION="${AUTOAGENTS_VERSION:-latest}"
BASE="${AUTOAGENTS_DOWNLOAD_BASE:-https://github.com/${REPO}/releases/download}"
INSTALL_DIR="${AUTOAGENTS_INSTALL_DIR:-${HOME}/.autoagents/bin}"
# GitHub 上「最新版」的地址是 releases/latest/download/<文件>，中间没有版本号那一层
# （releases/download/latest/ 是 404）。指定了别的下载地址时，照旧按 <地址>/<版本>/ 去取
dir="${BASE}/${VERSION}"
if [ "${VERSION}" = "latest" ] && [ "${BASE}" = "https://github.com/${REPO}/releases/download" ]; then
  dir="https://github.com/${REPO}/releases/latest/download"
fi

say() { printf '%s\n' "$*"; }
die() { printf '\n安装失败：%s\n\n' "$*" >&2; exit 1; }
need() {
  command -v "${1}" >/dev/null 2>&1 || die "这台机器上没有 ${1}，请先安装（${2}）"
}

# 下载到文件。子进程一律不给标准输入：脚本本身是从管道里读进来的
fetch() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --connect-timeout 15 --retry 3 -o "${2}" "${1}" </dev/null
  else
    wget -q -O "${2}" -T 15 "${1}" </dev/null
  fi
}

need tar "系统自带；精简容器里请安装 tar 与 gzip"
command -v curl >/dev/null 2>&1 || command -v wget >/dev/null 2>&1 \
  || die "这台机器上没有 curl 或 wget，请先安装其中一个"

# ---------- 1. 认出平台 ----------
os="$(uname -s)"
arch="$(uname -m)"
case "${os}" in
  Linux) flavor=linux ;;
  Darwin) flavor=darwin ;;
  *)
    case "${os}" in
      MINGW*|MSYS*|CYGWIN*) die "Windows 暂不支持。把 WSL（Ubuntu）装好后，在 WSL 里重新执行这条命令" ;;
      *) die "暂不支持 ${os}：agentd 目前提供 Linux 与 macOS 版本" ;;
    esac
    ;;
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

# ---------- 2. 下载并校验 ----------
work="$(mktemp -d 2>/dev/null || mktemp -d -t autoagents)"
trap 'rm -rf "$work"' EXIT INT TERM
asset="autoagents-node-${TARGET}.tar.gz"
url="${dir}/${asset}"
say "下载：${url}"
fetch "${url}" "${work}/${asset}" || die \
  "下载失败。如果地址打不开（404），说明 ${VERSION} 这个版本没有提供 ${TARGET} 的构建：
  https://github.com/${REPO}/releases
也可以用 AUTOAGENTS_VERSION 指定一个已有的版本。"

if [ "${AUTOAGENTS_NO_CHECKSUM:-0}" != "1" ]; then
  fetch "${dir}/SHA256SUMS" "${work}/SHA256SUMS" || die "下载校验文件失败"
  want="$(grep " ${asset}\$" "${work}/SHA256SUMS" | awk '{print $1}')"
  [ -n "${want}" ] || die "校验文件里没有 ${asset}，这个版本不完整"
  if command -v sha256sum >/dev/null 2>&1; then
    got="$(sha256sum "${work}/${asset}" | awk '{print $1}')"
  elif command -v shasum >/dev/null 2>&1; then
    got="$(shasum -a 256 "${work}/${asset}" | awk '{print $1}')"
  else
    die "这台机器上没有 sha256sum 或 shasum，无法校验下载的文件。确认来源可信的话，可以在命令前加 AUTOAGENTS_NO_CHECKSUM=1 跳过校验"
  fi
  if [ "${want}" != "${got}" ]; then
    die "校验不对（期望 ${want}，实际 ${got}）：文件可能损坏或被篡改。
如果正好赶上新版本发布，重试一次；或者用 AUTOAGENTS_VERSION 固定版本。"
  fi
  say "校验通过：${got}"
else
  say "已按 AUTOAGENTS_NO_CHECKSUM=1 跳过校验"
fi

# ---------- 3. 安装程序 ----------
mkdir -p "${INSTALL_DIR}"
tar -xzf "${work}/${asset}" -C "${work}" agentd
chmod 0755 "${work}/agentd"
mv -f "${work}/agentd" "${INSTALL_DIR}/agentd"
case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *) say "提示：${INSTALL_DIR} 不在 PATH 里，下文里的 agentd 请写成 ${INSTALL_DIR}/agentd" ;;
esac
"${INSTALL_DIR}/agentd" --version </dev/null

# ---------- 4. 配置并启动 ----------
case "${1:-}" in
  upgrade)
    say "已更新到 ${INSTALL_DIR}/agentd。正在运行的服务要重启一次才会用上新版本：
  macOS：launchctl kickstart -k gui/\$(id -u)/com.tianlong.autoagents.agentd
  Linux：systemctl --user restart autoagents-agentd.service"
    exit 0
    ;;
  "")
    exec "${INSTALL_DIR}/agentd" "$@" </dev/null
    ;;
esac
exec "${INSTALL_DIR}/agentd" "$@" </dev/null
