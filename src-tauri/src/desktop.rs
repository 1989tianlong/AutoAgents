//! 桌面 Agent 的进程管理：启动、退出、重启、运行状态。
//! 目前只支持本机（macOS）——agentd 上还没有通用的桌面控制通道，远程机器先明确拒绝。

use serde::Serialize;
use std::path::Path;
use std::sync::Arc;
use tauri::State;

use crate::AppState;
use crate::events;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopState {
    pub path: String,
    pub running: bool,
}

fn local_only(node_id: &str) -> Result<(), String> {
    if node_id != "local" {
        return Err("桌面 Agent 的控制目前只支持本机；其它机器要等 agentd 支持桌面控制".into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn mac_only() -> Result<(), String> {
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn mac_only() -> Result<(), String> {
    Err("桌面 Agent 的控制目前只支持 macOS".into())
}

/// 这个应用在跑吗：拿 .app 的完整路径去匹配进程命令行，只有它自己会命中
#[cfg(target_os = "macos")]
fn running(path: &str) -> bool {
    let needle = format!("{path}/");
    std::process::Command::new("pgrep")
        .arg("-f")
        .arg(&needle)
        .stdin(std::process::Stdio::null())
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
fn running(_path: &str) -> bool {
    false
}

/// 启动桌面应用：`open -g -j` 后台启动、不抢焦点
#[cfg(target_os = "macos")]
fn launch_inner(path: &str) -> Result<(), String> {
    if !Path::new(path).exists() {
        return Err(format!("应用不在了：{path}"));
    }
    let out = std::process::Command::new("open")
        .args(["-g", "-j", path])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("启动失败：{e}"))?;
    if !out.status.success() {
        return Err(format!(
            "启动失败：{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn launch_inner(_path: &str) -> Result<(), String> {
    Err("桌面 Agent 的控制目前只支持 macOS".into())
}

/// 退出桌面应用：先请它自己优雅退出（AppleScript），不奏效再按路径精确杀进程
#[cfg(target_os = "macos")]
fn quit_inner(path: &str, name: &str) -> Result<(), String> {
    if !running(path) {
        return Ok(());
    }
    let _ = std::process::Command::new("osascript")
        .args(["-e", &format!("tell application \"{name}\" to quit")])
        .stdin(std::process::Stdio::null())
        .output();
    // 给它一点时间自己退
    for _ in 0..20 {
        if !running(path) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    // 还不走：按 .app 路径精确匹配再杀（路径是唯一的，不会伤到别的进程）
    let needle = format!("{path}/");
    let _ = std::process::Command::new("pkill")
        .args(["-f", &needle])
        .stdin(std::process::Stdio::null())
        .output();
    for _ in 0..20 {
        if !running(path) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    Err("请它退出它不走，强制也没能停下；看看它是不是卡在了系统对话框上".into())
}

#[cfg(not(target_os = "macos"))]
fn quit_inner(_path: &str, _name: &str) -> Result<(), String> {
    Err("桌面 Agent 的控制目前只支持 macOS".into())
}

fn record_launch(state: &AppState, name: &str) {
    let db = state.db.lock().unwrap();
    events::record(
        &db,
        "agent.launch",
        "info",
        name,
        &format!("启动了 {name}"),
        None,
    );
}

fn record_quit(state: &AppState, name: &str) {
    let db = state.db.lock().unwrap();
    events::record(
        &db,
        "agent.quit",
        "info",
        name,
        &format!("退出了 {name}"),
        None,
    );
}

/// 本机所有桌面 Agent 的运行状态（Agents 页一次拉全）
pub fn states_inner() -> Result<Vec<DesktopState>, String> {
    let list = autoagents_core::discovery::desktop_apps();
    Ok(list
        .into_iter()
        .map(|agent| DesktopState {
            running: running(&agent.path),
            path: agent.path,
        })
        .collect())
}

pub fn launch(state: &AppState, node_id: &str, path: &str, name: &str) -> Result<(), String> {
    local_only(node_id)?;
    mac_only()?;
    launch_inner(path)?;
    record_launch(state, name);
    Ok(())
}

pub fn quit(state: &AppState, node_id: &str, path: &str, name: &str) -> Result<(), String> {
    local_only(node_id)?;
    mac_only()?;
    quit_inner(path, name)?;
    record_quit(state, name);
    Ok(())
}

pub fn restart(state: &AppState, node_id: &str, path: &str, name: &str) -> Result<(), String> {
    local_only(node_id)?;
    mac_only()?;
    quit_inner(path, name)?;
    launch_inner(path)?;
    let db = state.db.lock().unwrap();
    events::record(
        &db,
        "agent.launch",
        "info",
        name,
        &format!("重启了 {name}"),
        None,
    );
    Ok(())
}

#[tauri::command]
pub fn desktop_states() -> Result<Vec<DesktopState>, String> {
    states_inner()
}

#[tauri::command]
pub fn desktop_launch(
    state: State<'_, Arc<AppState>>,
    node_id: String,
    path: String,
    name: String,
) -> Result<(), String> {
    launch(&state, &node_id, &path, &name)
}

#[tauri::command]
pub fn desktop_quit(
    state: State<'_, Arc<AppState>>,
    node_id: String,
    path: String,
    name: String,
) -> Result<(), String> {
    quit(&state, &node_id, &path, &name)
}

#[tauri::command]
pub fn desktop_restart(
    state: State<'_, Arc<AppState>>,
    node_id: String,
    path: String,
    name: String,
) -> Result<(), String> {
    restart(&state, &node_id, &path, &name)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::testutil::test_state;
    use autoagents_core::service::{Service, ServiceOptions};

    /// 造一个最小的假 .app：可执行文件是个睡 60 秒的脚本
    fn fake_app(dir: &std::path::PathBuf, label: &str) -> String {
        let app = dir.join(format!("Fake{label}.app"));
        let macos = app.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleExecutable</key>\
             <string>run</string><key>CFBundleIdentifier</key><string>test.fake{label}</string></dict></plist>",
        )
        .unwrap();
        let bin = macos.join("run");
        std::fs::write(&bin, "#!/bin/sh\nsleep 60\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        app.to_string_lossy().into_owned()
    }

    #[test]
    fn remote_and_launch_cycle() {
        let ts = test_state(Service::new(ServiceOptions::default()));
        let state = ts.state.clone();

        // 远程机器明确拒绝
        assert!(launch(&state, "node-2", "/Applications/Fake.app", "Fake").is_err());
        assert!(quit(&state, "node-2", "/Applications/Fake.app", "Fake").is_err());
        assert!(restart(&state, "node-2", "/Applications/Fake.app", "Fake").is_err());
        // 不存在的应用起不来
        assert!(launch(&state, "local", "/Applications/不存在.app", "不存在").is_err());

        let dir = std::env::temp_dir().join(format!("aa-desktop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let app = fake_app(&dir, "One");

        // 启动 → 在跑（open 要一点时间拉起进程）
        launch(&state, "local", &app, "FakeOne").unwrap();
        let mut up = false;
        for _ in 0..40 {
            if running(&app) {
                up = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        assert!(up, "启动后应该在跑");

        // 退出（假 app 听不懂 AppleScript，走的是精确匹配强停的兜底路径）
        quit(&state, "local", &app, "FakeOne").unwrap();
        assert!(!running(&app), "退出后应该不在跑");
        // 已经不在跑：再退一次是无害的
        quit(&state, "local", &app, "FakeOne").unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }
}
