mod automation;
mod chat;
mod db;
mod deploy;
mod desktop;
mod discovery;
mod events;
mod github;
mod inbox;
mod models;
mod nodes;
mod pipeline;
mod remote;
mod secrets;
mod tasks;
mod usage;
mod workflow;

#[cfg(all(test, unix))]
mod testutil;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::Manager;
use tokio::sync::mpsc;

/// 全局应用状态：SQLite 连接、本机的机器侧服务、运行中的对话、远程机器、凭据存储。
pub struct AppState {
    pub db: Arc<Mutex<rusqlite::Connection>>,
    /// 本机的机器侧服务：与远程节点上 agentd 提供的是同一个服务
    pub service: Arc<autoagents_core::service::Service>,
    /// 对话事件总线：本机与各节点的过程事件都汇到这里，按会话归档
    pub events: mpsc::UnboundedSender<chat::NodeEvent>,
    /// 运行中的一轮对话及其实时内容（界面据此校正状态）
    pub live: Mutex<HashMap<i64, chat::LiveTurn>>,
    /// 把会话事件推给界面
    pub sink: Arc<dyn chat::Sink>,
    /// 凭据（系统 Keychain）：GitHub PAT、中继令牌、与各机器配对的密钥、部署平台令牌
    pub secrets: Arc<dyn secrets::SecretStore>,
    /// 远程机器：配对、连接、状态与请求转发
    pub remote: Arc<remote::RemoteManager>,
    /// 本机的机器信息：第一次取到后沿用
    pub local_info: Mutex<Option<autoagents_core::machine::MachineInfo>>,
    /// GitHub API 的地址（测试里指向本地的假服务器）
    pub gh_api: Mutex<String>,
    /// Vercel / Cloudflare API 的地址（测试里指向本地的假服务器）
    pub vercel_api: Mutex<String>,
    pub cf_api: Mutex<String>,
    /// 部署模块的运行时状态（正在刷新的目标）
    pub deploy: deploy::Deployer,
    /// 任务流水线的运行时状态（正在收尾的任务）
    pub pipeline: pipeline::Pipeline,
    /// 任务流水线挂在会话上的钩子
    pub turn_hooks: Mutex<chat::TurnHooks>,
}

impl AppState {
    /// 组装状态并启动事件归档任务。
    pub fn new(
        db: rusqlite::Connection,
        service: Arc<autoagents_core::service::Service>,
        sink: Arc<dyn chat::Sink>,
        node_sink: Arc<dyn remote::NodeSink>,
        secrets: Arc<dyn secrets::SecretStore>,
    ) -> Arc<Self> {
        let (events, events_rx) = mpsc::unbounded_channel();
        let db = Arc::new(Mutex::new(db));
        let remote = remote::RemoteManager::new(
            Arc::clone(&db),
            Arc::clone(&secrets),
            events.clone(),
            node_sink,
        );
        let state = Arc::new(Self {
            db,
            service,
            events,
            live: Mutex::new(HashMap::new()),
            sink,
            secrets,
            remote: Arc::new(remote),
            local_info: Mutex::new(None),
            gh_api: Mutex::new(github::DEFAULT_API.to_string()),
            vercel_api: Mutex::new(deploy::VERCEL_API.to_string()),
            cf_api: Mutex::new(deploy::CF_API.to_string()),
            deploy: deploy::Deployer::default(),
            pipeline: pipeline::Pipeline::default(),
            turn_hooks: Mutex::new(chat::TurnHooks::default()),
        });
        chat::dispatch(state.clone(), events_rx);
        // 一轮对话开始、结束时通知任务流水线（挂在会话上的钩子，同样拿弱引用）
        {
            let (started, ended) = (Arc::downgrade(&state), Arc::downgrade(&state));
            *state.turn_hooks.lock().unwrap() = chat::TurnHooks {
                started: Some(Arc::new(move |session_id| {
                    if let Some(state) = started.upgrade() {
                        pipeline::turn_started(&state, session_id);
                    }
                })),
                ended: Some(Arc::new(move |session_id, ok| {
                    if let Some(state) = ended.upgrade() {
                        pipeline::turn_ended(&state, session_id, ok);
                    }
                })),
            };
        }
        // 机器掉线时结束它上面正在运行的对话。拿弱引用，免得状态与自己互相引用
        let weak = Arc::downgrade(&state);
        state.remote.set_on_offline(Arc::new(move |node| {
            if let Some(state) = weak.upgrade() {
                chat::fail_node_sessions(&state, node);
            }
        }));
        state
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
            std::fs::create_dir_all(&dir).map_err(|e| format!("创建数据目录失败: {e}"))?;
            let conn = db::init(&dir.join("autoagents.db"))?;
            let service =
                autoagents_core::service::Service::new(autoagents_core::service::ServiceOptions {
                    flavor: "桌面应用内置",
                    ..Default::default()
                });
            let state = AppState::new(
                conn,
                service,
                Arc::new(chat::TauriSink(app.handle().clone())),
                Arc::new(remote::TauriNodeSink(app.handle().clone())),
                Arc::new(secrets::KeychainStore),
            );
            app.manage(state.clone());
            // 连上配对过的机器、开始跟踪中继（都在后台，不拖慢启动）
            state.remote.start_all();
            // 接着跟踪各个任务的 PR、各个项目的部署、工作流的推进，以及自动化规则
            pipeline::start_polling(state.clone());
            deploy::start_polling(state.clone());
            workflow::start_polling(state.clone());
            automation::start_polling(state.clone());
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
            tasks::task_get,
            tasks::task_create_issue,
            tasks::task_create_manual,
            tasks::task_set_status,
            tasks::task_delete,
            // 工作流编排
            workflow::workflow_templates,
            workflow::workflow_create,
            workflow::workflow_list,
            workflow::workflow_get,
            workflow::workflow_start,
            workflow::workflow_cancel,
            workflow::workflow_gate,
            workflow::workflow_retry,
            // 自动化规则与决策中心
            automation::automation_rules,
            automation::automation_runs,
            automation::automation_rule_create,
            automation::automation_rule_set_enabled,
            automation::automation_rule_delete,
            automation::automation_scan,
            inbox::inbox_list,
            inbox::inbox_merge_pr,
            // 任务流水线
            pipeline::task_start,
            pipeline::task_submit,
            pipeline::task_fix,
            pipeline::task_refresh,
            pipeline::tasks_refresh,
            pipeline::task_abort,
            pipeline::task_diff,
            pipeline::task_diff_file,
            pipeline::repo_clone,
            // 部署
            desktop::desktop_states,
            desktop::desktop_launch,
            desktop::desktop_quit,
            desktop::desktop_restart,
            deploy::deploy_status,
            deploy::deploy_connect,
            deploy::deploy_disconnect,
            deploy::deploy_set_scope,
            deploy::deploy_discover,
            deploy::deploy_targets,
            deploy::deploy_target_add,
            deploy::deploy_target_update,
            deploy::deploy_target_remove,
            deploy::deploy_list,
            deploy::deploy_refresh,
            deploy::deploy_trigger,
            deploy::deploy_retry,
            deploy::deploy_cancel,
            deploy::deploy_promote,
            deploy::deploy_rollback,
            deploy::deploy_history,
            deploy::deploy_logs,
            deploy::deploy_check_health,
            deploy::deploy_for_task,
            models::agent_models,
            models::model_remember,
            chat::chat_sessions,
            chat::chat_messages,
            chat::chat_start,
            chat::chat_send,
            chat::chat_stop,
            chat::chat_live,
            // 机器
            agents_all,
            machines_list,
            node_metrics,
            node_ls,
            node_git,
            remote::relay_get,
            remote::relay_set,
            remote::relay_clear,
            remote::relay_status,
            remote::install_script_set,
            remote::node_install_command,
            remote::node_pair,
            remote::node_pair_direct,
            remote::node_authorize,
            remote::nodes_list,
            remote::node_remove,
            remote::node_rename,
            remote::node_connect_now,
            // 活动与审计
            events::events_list,
            events::events_counts,
            events::events_clear,
            // 用量与成本
            usage::usage_summary,
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

use nodes::{agents_all, machines_list, node_git, node_ls, node_metrics};
