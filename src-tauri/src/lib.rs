mod discovery;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            discovery::app_info,
            discovery::discover_agents
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
