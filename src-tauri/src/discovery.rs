//! 本机 Agent 发现：扫描 PATH 中的 CLI Agent 与 /Applications 中的桌面 Agent。
//! 多机（agentd）发现将在 M2 引入。

use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub os: String,
    pub arch: String,
    pub hostname: String,
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
const DESKTOP_PATTERNS: &[(&str, &str, &str)] = &[
    ("kimi code", "Kimi Code", "kimi-code"),
    ("grok bot", "Grok Bot", "grok-bot"),
    ("zcode", "ZCode", "zcode"),
    ("codex", "Codex Desktop", "codex-desktop"),
    ("claude", "Claude Desktop", "claude-desktop"),
];

pub(crate) fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    let home = std::env::var("HOME").unwrap_or_default();
    // 常见安装位置兜底（PATH 未覆盖时）
    for extra in [
        format!("{home}/.local/bin"),
        "/opt/homebrew/bin".to_string(),
        "/usr/local/bin".to_string(),
    ] {
        if !dirs.iter().any(|d| d.to_string_lossy() == extra) {
            dirs.push(PathBuf::from(extra));
        }
    }
    dirs
}

pub(crate) fn find_binary(binary: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter().find_map(|d| {
        let p = d.join(binary);
        p.is_file().then_some(p)
    })
}

/// 探测版本号：执行 `<bin> --version`，3 秒超时，取首行。
async fn probe_version(bin: &Path) -> Option<String> {
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tokio::process::Command::new(bin).arg("--version").output(),
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

#[cfg(target_os = "macos")]
fn scan_desktop_apps() -> Vec<DiscoveredAgent> {
    let home = std::env::var("HOME").unwrap_or_default();
    let roots = [
        PathBuf::from("/Applications"),
        PathBuf::from(home).join("Applications"),
    ];
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
            if let Some((_, display, adapter)) =
                DESKTOP_PATTERNS.iter().find(|(kw, _, _)| lower.contains(kw))
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
fn scan_desktop_apps() -> Vec<DiscoveredAgent> {
    Vec::new()
}

#[tauri::command]
pub fn app_info() -> AppInfo {
    let hostname = std::process::Command::new("hostname")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "localhost".into());
    AppInfo {
        name: env!("CARGO_PKG_NAME").into(),
        version: env!("CARGO_PKG_VERSION").into(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        hostname,
    }
}

/// 扫描本机：PATH 中的 CLI Agent + /Applications 中的桌面 Agent。
#[tauri::command]
pub async fn discover_agents() -> Result<Vec<DiscoveredAgent>, String> {
    let dirs = search_dirs();
    let mut agents = Vec::new();
    for (bin, display, adapter, level) in CLI_AGENTS {
        if let Some(path) = find_binary(bin, &dirs) {
            let version = probe_version(&path).await;
            agents.push(DiscoveredAgent {
                id: format!("local/{adapter}"),
                name: (*display).into(),
                kind: "cli".into(),
                level: (*level).into(),
                adapter: (*adapter).into(),
                path: path.to_string_lossy().into_owned(),
                version,
            });
        }
    }
    agents.extend(scan_desktop_apps());
    Ok(agents)
}
