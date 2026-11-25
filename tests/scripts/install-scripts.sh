#!/bin/sh
# 两个安装脚本的回归测试：用替身的 curl / wget 假装 GitHub Releases，把脚本各跑几遍，不联网。
#
# 用法：sh tests/scripts/install-scripts.sh
#
# 假的 GitHub 照真实规则响应：
#   releases/latest/download/<文件>     最新版
#   releases/download/<版本>/<文件>     指定版本
#   releases/download/latest/<文件>     404 —— 默认取「最新版」时最容易写错的地方
#
# 脚本里会执行下载下来的 agentd / autoagents-relay，所以压缩包里放的是只会回应 --version 的假程序。

set -eu

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SH="$(command -v sh)"
REPO="1989tianlong/AutoAgents"
GITHUB="https://github.com/${REPO}/releases"
work="$(mktemp -d 2>/dev/null || mktemp -d -t autoagents-scripts)"
trap 'rm -rf "${work}"' EXIT INT TERM

failures=0
total=0
# check <说明> <退出码：0 算通过> [出错时附在后面的信息]
check() {
  total=$((total + 1))
  if [ "$2" = 0 ]; then
    echo "  ✓ $1"
  else
    echo "  ✗ $1${3:+  ← $3}"
    failures=$((failures + 1))
  fi
}
heading() { printf '\n%s\n' "$1"; }

# ---------- 假的发布 ----------
os="$(uname -s)"
arch="$(uname -m)"
case "${arch}" in x86_64|amd64) cpu=x86_64 ;; aarch64|arm64) cpu=aarch64 ;; esac
case "${os}-${cpu}" in
  Linux-x86_64) TARGET=x86_64-unknown-linux-musl ;;
  Linux-aarch64) TARGET=aarch64-unknown-linux-musl ;;
  Darwin-x86_64) TARGET=x86_64-apple-darwin ;;
  Darwin-aarch64) TARGET=aarch64-apple-darwin ;;
  *) echo "这台机器（${os} ${arch}）没有对应的构建，测不了" >&2; exit 1 ;;
esac
ASSET="autoagents-node-${TARGET}.tar.gz"

# 和真实发布一样：每行是「校验值  文件名」，文件名不带路径
sum_file() {
  (cd "$(dirname "$1")" && f="$(basename "$1")" && {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$f" | awk '{print $1"  "$2}'
    else shasum -a 256 "$f"; fi
  })
}

# 在 <目录> 里造一个版本：压缩包与校验值；里面的假程序回应 --version 时报 <版本>
make_release() {
  mkdir -p "$1/stage"
  printf '#!/bin/sh\necho "agentd %s"\n' "$2" > "$1/stage/agentd"
  printf '#!/bin/sh\necho "autoagents-relay %s"\n' "$2" > "$1/stage/autoagents-relay"
  chmod 0755 "$1/stage/agentd" "$1/stage/autoagents-relay"
  COPYFILE_DISABLE=1 tar -czf "$1/${ASSET}" -C "$1/stage" agentd autoagents-relay
  sum_file "$1/${ASSET}" > "$1/SHA256SUMS"
}

FAKE="${work}/fake"
make_release "${FAKE}/latest" 9.9.9
make_release "${FAKE}/v8.8.8" 8.8.8
make_release "${FAKE}/mirror/v7.7.7" 7.7.7
ln -s ../latest "${FAKE}/mirror/latest"

# ---------- 替身的 curl / wget ----------
cat > "${work}/serve.sh" <<'STUB'
# 按假 GitHub 的规则把 <地址> 对应的文件写到 <输出>；取不到就退出码 22（curl 遇到 HTTP 错误时的退出码）
serve() {
  url="$1"
  out="$2"
  prefix="https://github.com/${REPO}/releases/download/"
  case "${url}" in
    "https://github.com/${REPO}/releases/latest/download/"*)
      file="${FAKE}/latest/${url##*/}" ;;
    "${prefix}"*)
      rest="${url#"${prefix}"}"
      [ "${rest%%/*}" = "latest" ] && exit 22
      file="${FAKE}/${rest}" ;;
    file://*)
      file="${url#file://}" ;;
    *)
      exit 22 ;;
  esac
  [ -f "${file}" ] || exit 22
  cp "${file}" "${out}"
}
STUB

cat > "${work}/curl" <<'STUB'
#!/bin/sh
# 替身 curl：认 `curl … -o <输出> <地址>`
. "${STUB_LIB}"
out=""
url=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) shift; out="$1" ;;
    http://*|https://*|file://*) url="$1" ;;
  esac
  shift
done
printf '%s\n' "${url}" >> "${CURL_LOG}"
serve "${url}" "${out}"
STUB

cat > "${work}/wget" <<'STUB'
#!/bin/sh
# 替身 wget：认 `wget … -O <输出> … <地址>`
. "${STUB_LIB}"
out=""
url=""
while [ $# -gt 0 ]; do
  case "$1" in
    -O) shift; out="$1" ;;
    http://*|https://*|file://*) url="$1" ;;
  esac
  shift
done
printf '%s\n' "${url}" >> "${CURL_LOG}"
serve "${url}" "${out}"
STUB
chmod 0755 "${work}/curl" "${work}/wget"

# 被测脚本只能看到替身的下载工具：给它们一份收窄的 PATH，需要的系统命令链进来，别的不给
make_path() {
  mkdir -p "$1"
  for tool in uname mktemp tar gzip rm mv chmod mkdir awk grep sed dirname id cp; do
    path="$(command -v "${tool}" 2>/dev/null || true)"
    [ -n "${path}" ] && ln -sf "${path}" "$1/${tool}"
  done
  if command -v sha256sum >/dev/null 2>&1; then ln -sf "$(command -v sha256sum)" "$1/sha256sum"
  else ln -sf "$(command -v shasum)" "$1/shasum"; fi
}
make_path "${work}/with-curl"
ln -sf "${work}/curl" "${work}/with-curl/curl"
make_path "${work}/with-wget"
ln -sf "${work}/wget" "${work}/with-wget/wget"

export REPO FAKE STUB_LIB="${work}/serve.sh" CURL_LOG="${work}/urls.log"

# ---------- 跑被测的脚本 ----------
# attempt <with-curl | with-wget> [NAME=value]… -- <脚本名> [参数]…
# 输出放进 ${work}/out.log，退出码放进 ${code}；这次请求过的地址在 ${CURL_LOG}
attempt() {
  tool_path="${work}/$1"
  shift
  extra=""
  while [ "${1}" != "--" ]; do extra="${extra} $1"; shift; done
  shift
  script="${REPO_ROOT}/scripts/$1"
  shift
  : > "${CURL_LOG}"
  rm -rf "${work}/inst"
  set +e
  # shellcheck disable=SC2086  # extra 就是要拆成一个个 NAME=value；sh 用绝对路径，收窄的 PATH 里找不到它
  env PATH="${tool_path}" HOME="${work}/home" AUTOAGENTS_INSTALL_DIR="${work}/inst" ${extra} \
    "${SH}" "${script}" "$@" </dev/null > "${work}/out.log" 2>&1
  code=$?
  set -e
}

# 这次运行请求过的地址，是否恰好就是给的那几个
expect_urls() {
  [ "$(wc -l < "${CURL_LOG}" | tr -d ' ')" = "$#" ] || return 1
  for url in "$@"; do grep -qFx "${url}" "${CURL_LOG}" || return 1; done
}
# 装下来的程序回应 --version 时是不是这个版本
expect_version() {
  [ "$("${work}/inst/$1" --version 2>/dev/null)" = "$1 $2" ]
}
out_grep() { grep -q "$1" "${work}/out.log"; }
tail_of_output() { tail -3 "${work}/out.log" | tr '\n' ' '; }

heading "[1] agentd：默认取最新版"
attempt with-curl -- install-agentd.sh upgrade
check "脚本跑通" "${code}" "$(tail_of_output)"
check "装下来的程序能运行，是最新版" "$(expect_version agentd 9.9.9; echo $?)"
check "走的是 GitHub 上「最新版」的地址" \
  "$(expect_urls "${GITHUB}/latest/download/${ASSET}" "${GITHUB}/latest/download/SHA256SUMS"; echo $?)" "$(tr '\n' ' ' < "${CURL_LOG}")"

heading "[2] agentd：指定版本"
attempt with-curl AUTOAGENTS_VERSION=v8.8.8 -- install-agentd.sh upgrade
check "脚本跑通，装的是指定的版本" "$( [ "${code}" = 0 ] && expect_version agentd 8.8.8; echo $?)"
check "地址里带版本号" \
  "$(expect_urls "${GITHUB}/download/v8.8.8/${ASSET}" "${GITHUB}/download/v8.8.8/SHA256SUMS"; echo $?)" "$(tr '\n' ' ' < "${CURL_LOG}")"

heading "[3] 只有 wget 的机器"
attempt with-wget -- install-relay.sh --public wss://relay.example.com:7000
check "脚本跑通" "${code}" "$(tail_of_output)"
check "装下来的程序能运行，是最新版" "$(expect_version autoagents-relay 9.9.9; echo $?)"
check "同样走「最新版」的地址" \
  "$(expect_urls "${GITHUB}/latest/download/${ASSET}" "${GITHUB}/latest/download/SHA256SUMS"; echo $?)" "$(tr '\n' ' ' < "${CURL_LOG}")"

heading "[4] 内网镜像"
MIRROR="AUTOAGENTS_DOWNLOAD_BASE=file://${FAKE}/mirror"
attempt with-curl "${MIRROR}" AUTOAGENTS_VERSION=v7.7.7 -- install-agentd.sh upgrade
check "指定版本：按 <地址>/<版本>/ 去取" \
  "$( [ "${code}" = 0 ] && expect_version agentd 7.7.7 && expect_urls "file://${FAKE}/mirror/v7.7.7/${ASSET}" "file://${FAKE}/mirror/v7.7.7/SHA256SUMS"; echo $?)"
attempt with-curl "${MIRROR}" -- install-agentd.sh upgrade
check "取最新版：镜像上照旧是 <地址>/latest/（那里没有 GitHub 的 latest/download）" \
  "$( [ "${code}" = 0 ] && expect_version agentd 9.9.9 && expect_urls "file://${FAKE}/mirror/latest/${ASSET}" "file://${FAKE}/mirror/latest/SHA256SUMS"; echo $?)"

heading "[5] 校验不过就不装"
mv "${FAKE}/latest/${ASSET}" "${FAKE}/latest/keep"
make_release "${FAKE}/scratch" 0.0.0
cp "${FAKE}/scratch/${ASSET}" "${FAKE}/latest/${ASSET}"
attempt with-curl -- install-agentd.sh upgrade
check "文件和校验值对不上：脚本失败，并说明原因" "$( [ "${code}" != 0 ] && out_grep "校验不对"; echo $?)" "$(tail_of_output)"
check "没把对不上的文件装进去" "$([ ! -e "${work}/inst/agentd" ]; echo $?)"
mv "${FAKE}/latest/keep" "${FAKE}/latest/${ASSET}"

heading "[6] 版本不存在"
attempt with-curl AUTOAGENTS_VERSION=v0.0.1 -- install-agentd.sh upgrade
check "下载不到就失败，并提示可以换一个版本" \
  "$( [ "${code}" != 0 ] && out_grep "下载失败" && out_grep "AUTOAGENTS_VERSION"; echo $?)" "$(tail_of_output)"

echo ""
if [ "${failures}" = 0 ]; then
  echo "${total}/${total} 项通过"
else
  echo "总数 ${total}，没通过 ${failures}"
fi
[ "${failures}" = 0 ]
