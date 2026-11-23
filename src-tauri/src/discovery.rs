//! 本机信息与 Agent 发现：Tauri 命令层，实现在 `autoagents_core`。

use autoagents_core::discovery as core;
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub os: String,
    pub arch: String,
    pub hostname: String,
    pub home: String,
}

#[tauri::command]
pub fn app_info() -> AppInfo {
    AppInfo {
        name: env!("CARGO_PKG_NAME").into(),
        version: env!("CARGO_PKG_VERSION").into(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        hostname: core::hostname(),
        home: core::home_dir().to_string_lossy().into_owned(),
    }
}

/// 扫描本机：PATH 中的 CLI Agent + 桌面应用目录里的桌面 Agent。
#[tauri::command]
pub async fn discover_agents() -> Result<Vec<core::DiscoveredAgent>, String> {
    Ok(core::discover().await)
}
