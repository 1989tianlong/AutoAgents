mod adapters;
mod chat;
mod db;
mod discovery;
mod github;
mod models;
mod tasks;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tauri::Manager;

/// 全局应用状态：SQLite 连接、GitHub 连接状态、运行中的 Agent 会话。
pub struct AppState {
    pub db: Mutex<rusqlite::Connection>,
    pub gh: Mutex<Option<github::GithubState>>,
    /// 运行中的 Agent 子进程：会话 id → 子进程
    running: Mutex<HashMap<i64, tokio::process::Child>>,
    /// 已请求停止的会话
    cancelled: Mutex<HashSet<i64>>,
    /// 运行中的一轮对话及其实时内容（界面据此校正状态）
    live: Mutex<HashMap<i64, chat::LiveTurn>>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
            std::fs::create_dir_all(&dir).map_err(|e| format!("创建数据目录失败: {e}"))?;
            let conn = db::init(&dir.join("autoagents.db"))?;
            app.manage(Arc::new(AppState {
                db: Mutex::new(conn),
                gh: Mutex::new(None),
                running: Mutex::new(HashMap::new()),
                cancelled: Mutex::new(HashSet::new()),
                live: Mutex::new(HashMap::new()),
            }));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            discovery::app_info,
            discovery::discover_agents,
            github::github_connect,
            github::github_status,
            github::github_disconnect,
            github::github_repos,
            github::github_issues,
            github::github_prs,
            tasks::tasks_list,
            tasks::task_create_issue,
            tasks::task_set_status,
            tasks::task_delete,
            models::agent_models,
            models::model_remember,
            chat::chat_sessions,
            chat::chat_messages,
            chat::chat_start,
            chat::chat_send,
            chat::chat_stop,
            chat::chat_live,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            #[cfg(not(target_os = "macos"))]
            let _ = (app, event);
            // macOS：关掉全部窗口后进程仍在运行，点击 Dock 图标时默认什么也不会发生。
            // 这里把主窗口重新打开（还在的话就显示出来），省得只能重启应用。
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen {
                has_visible_windows: false,
                ..
            } = event
            {
                match app.get_webview_window("main") {
                    Some(window) => {
                        let _ = window.unminimize();
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                    None => {
                        let config = app.config();
                        let Some(window) = config.app.windows.iter().find(|w| w.label == "main")
                        else {
                            return;
                        };
                        let rebuilt = tauri::WebviewWindowBuilder::from_config(app, window)
                            .and_then(|builder| builder.build());
                        if let Err(e) = rebuilt {
                            eprintln!("[app] 重新打开主窗口失败：{e}");
                        }
                    }
                }
            }
        });
}
