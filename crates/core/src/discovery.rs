//! 本机 Agent 发现：扫描 PATH 中的 CLI Agent 与桌面应用目录。应用与 agentd 共用。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredAgent {
    pub id: String,
    pub name: String,
    /// "cli" | "desktop"
    pub kind: String,
    /// "L1" | "L2" | "L3"
    pub level: String,
    pub adapter: String,
    pub path: String,
    pub version: Option<String>,
}

/// 内置 CLI Agent 清单：(二进制名, 展示名, adapter, 能力等级)
const CLI_AGENTS: &[(&str, &str, &str, &str)] = &[
    ("claude", "Claude CLI", "claude-cli", "L3"),
    ("codex", "Codex CLI", "codex-cli", "L3"),
    ("grok", "Grok CLI", "grok-cli", "L3"),
    ("pi", "Pi Agent", "pi", "L3"),
    ("wrangler", "Wrangler (Cloudflare)", "wrangler", "L2"),
];

/// macOS 桌面 Agent 识别：(名称关键字, 展示名, adapter)。
/// 顺序即优先级：更具体的名称在前，避免聊天类 App（如 Kimi.app）抢占匹配。
#[cfg(target_os = "macos")]
const DESKTOP_PATTERNS: &[(&str, &str, &str)] = &[
    ("kimi code", "Kimi Code", "kimi-code"),
    ("grok bot", "Grok Bot", "grok-bot"),
    ("zcode", "ZCode", "zcode"),
    ("codex", "Codex Desktop", "codex-desktop"),
    ("claude", "Claude Desktop", "claude-desktop"),
];

/// 用户主目录；也是会话工作目录的默认值。
pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|home| !home.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 主机名；取不到时退回「localhost」
pub fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "localhost".into())
}

/// 查找可执行文件的目录：PATH + 常见安装位置。
/// 从 Dock / Finder / 开机自启启动的程序拿不到 shell 的 PATH，
/// 所以 nvm、homebrew 这些目录必须在这里兜底。
pub fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        })
        .unwrap_or_default();
    let home = home_dir();
    let mut extra: Vec<PathBuf> = [
        home.join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        home.join(".grok/bin"),
        home.join(".bun/bin"),
        home.join(".cargo/bin"),
    ]
    .into_iter()
    .collect();
    #[cfg(windows)]
    extra.push(home.join("AppData/Roaming/npm"));
    // nvm 管理的 node：按版本号降序，优先命中较新版本里安装的 CLI
    if let Ok(versions) = std::fs::read_dir(home.join(".nvm/versions/node")) {
        let mut nv: Vec<(Vec<u64>, PathBuf)> = versions
            .flatten()
            .map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let key = name
                    .trim_start_matches('v')
                    .split('.')
                    .filter_map(|part| part.parse::<u64>().ok())
                    .collect();
                (key, entry.path().join("bin"))
            })
            .collect();
        nv.sort_by(|a, b| b.0.cmp(&a.0));
        extra.extend(nv.into_iter().map(|(_, bin)| bin));
    }
    for dir in extra {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// 拉起子进程时使用的 PATH：当前 PATH 补上 [`search_dirs`]。
/// npm 安装的 CLI（claude / codex / pi）靠 `#!/usr/bin/env node` 启动，
/// GUI 应用的 PATH 里没有 node，不补齐会直接起不来。
pub fn child_path() -> std::ffi::OsString {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut paths: Vec<PathBuf> = std::env::split_paths(&current).collect();
    for dir in search_dirs() {
        if !paths.contains(&dir) {
            paths.push(dir);
        }
    }
    std::env::join_paths(paths).unwrap_or_else(|_| current.clone())
}

/// Windows 上 npm 安装的 CLI 实际是 `claude.cmd`，按 PATHEXT 的常见后缀找
#[cfg(windows)]
pub fn find_binary(binary: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let names: Vec<String> = std::iter::once(binary.to_string())
        .chain(
            ["cmd", "exe", "bat"]
                .iter()
                .map(|ext| format!("{binary}.{ext}")),
        )
        .collect();
    dirs.iter().find_map(|d| {
        names.iter().find_map(|name| {
            let p = d.join(name);
            p.is_file().then_some(p)
        })
    })
}

#[cfg(not(windows))]
pub fn find_binary(binary: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter().find_map(|d| {
        let p = d.join(binary);
        p.is_file().then_some(p)
    })
}

/// 探测版本号：执行 `<bin> --version`，3 秒超时，取首行。
async fn probe_version(bin: &Path) -> Option<String> {
    use std::process::Stdio;
    let out = tokio::time::timeout(
        Duration::from_secs(3),
        tokio::process::Command::new(bin)
            .arg("--version")
            .env("PATH", child_path())
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if line.is_empty() {
        None
    } else {
        Some(line.chars().take(80).collect())
    }
}

/// 桌面 Agent 清单（同步、只扫应用目录）：桌面控制要按路径查运行状态用
#[cfg(target_os = "macos")]
pub fn desktop_apps() -> Vec<DiscoveredAgent> {
    let home = home_dir();
    let roots = [PathBuf::from("/Applications"), home.join("Applications")];
    let mut agents = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if !file_name.ends_with(".app") {
                continue;
            }
            let lower = file_name.to_lowercase();
            if let Some((_, display, adapter)) = DESKTOP_PATTERNS
                .iter()
                .find(|(kw, _, _)| lower.contains(kw))
            {
                let path = entry.path().to_string_lossy().into_owned();
                // 同一 adapter 或同一路径只保留第一个匹配
                if agents
                    .iter()
                    .any(|a: &DiscoveredAgent| a.adapter == *adapter || a.path == path)
                {
                    continue;
                }
                agents.push(DiscoveredAgent {
                    id: format!("local/{adapter}"),
                    name: (*display).to_string(),
                    kind: "desktop".into(),
                    level: "L1".into(),
                    adapter: adapter.to_string(),
                    path,
                    version: None,
                });
            }
        }
    }
    agents
}

#[cfg(not(target_os = "macos"))]
pub fn desktop_apps() -> Vec<DiscoveredAgent> {
    Vec::new()
}

/// 扫描本机：PATH 中的 CLI Agent + 桌面应用目录里的桌面 Agent。
pub async fn discover() -> Vec<DiscoveredAgent> {
    let dirs = search_dirs();
    // 版本探测可能要等满超时，各 Agent 并行进行
    let probes: Vec<_> = CLI_AGENTS
        .iter()
        .filter_map(|(bin, display, adapter, level)| {
            let path = find_binary(bin, &dirs)?;
            Some(async move {
                let version = probe_version(&path).await;
                DiscoveredAgent {
                    id: format!("local/{adapter}"),
                    name: (*display).into(),
                    kind: "cli".into(),
                    level: (*level).into(),
                    adapter: (*adapter).into(),
                    path: path.to_string_lossy().into_owned(),
                    version,
                }
            })
        })
        .collect();
    let mut agents = futures_util::future::join_all(probes).await;
    agents.extend(desktop_apps());
    agents
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 必须拒绝同名目录（PATH 里混进目录时不能当成可执行文件）
    #[test]
    fn find_binary_skips_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("fake")).unwrap();
        assert!(find_binary("fake", &[dir.path().to_path_buf()]).is_none());
    }

    #[test]
    fn home_is_never_empty() {
        assert!(!home_dir().as_os_str().is_empty());
    }
}
