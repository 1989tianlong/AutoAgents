mod chat;
mod db;
mod discovery;
mod github;
mod tasks;

use std::sync::{Arc, Mutex};
use tauri::Manager;

/// 全局应用状态：SQLite 连接 + GitHub 连接状态。
pub struct AppState {
    pub db: Mutex<rusqlite::Connection>,
    pub gh: Mutex<Option<github::GithubState>>,
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
            chat::chat_sessions,
            chat::chat_messages,
            chat::chat_start,
            chat::chat_send,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
