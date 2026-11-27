//! 节点：控制端对「某台机器」的统一入口。
//!
//! 本机是一个内嵌的节点（`local`，机器侧服务就在应用进程里）；
//! 远程机器由 agentd 提供同一个服务，请求经加密通道转发过去。
//! 上层模块只认「节点 id + 请求」，不关心它在本地还是远程。

use crate::AppState;
use crate::chat::NodeEvent;
use autoagents_core::discovery::DiscoveredAgent;
use autoagents_core::machine::{MachineInfo, Metrics};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use tauri::State;
use tokio::sync::mpsc;

/// 本机的显示名
pub const LOCAL_NAME: &str = "本机";

/// 本机节点的 id
pub const LOCAL: &str = "local";

/// 空值一律指本机
pub fn normalize(node_id: Option<&str>) -> String {
    node_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(LOCAL)
        .to_string()
}

pub fn is_local(node_id: &str) -> bool {
    node_id == LOCAL
}

/// 向一台机器发请求并等待结果。`turn.start` 之后的对话事件不从这里返回，
/// 而是统一汇入事件总线（[`crate::chat::NodeEvent`]，带上来源机器）。
pub async fn call(
    state: &Arc<AppState>,
    node_id: &str,
    method: &str,
    params: &Value,
) -> Result<Value, String> {
    if is_local(node_id) {
        let (tx, mut rx) = mpsc::channel(256);
        let bus = state.events.clone();
        let node = node_id.to_string();
        tauri::async_runtime::spawn(async move {
            while let Some(event) = rx.recv().await {
                let _ = bus.send(NodeEvent {
                    node: node.clone(),
                    event,
                });
            }
        });
        state.service.call(method, params, tx).await
    } else {
        crate::remote::call(state, node_id, method, params.clone()).await
    }
}

/// 机器是否可用，以及（给了目录时）这个工作目录在那台机器上是否存在。
/// 开新会话前先校验，省得留下一开口就失败的会话。
pub async fn ensure_ready(
    state: &Arc<AppState>,
    node_id: &str,
    workdir: Option<&str>,
) -> Result<(), String> {
    let workdir = workdir.map(str::trim).filter(|s| !s.is_empty());
    if is_local(node_id) {
        return autoagents_core::runner::resolve_workdir(workdir).map(|_| ());
    }
    let params = match workdir {
        Some(path) => serde_json::json!({ "path": path }),
        None => serde_json::json!({}),
    };
    let method = if workdir.is_some() {
        "fs.ls"
    } else {
        "sys.info"
    };
    call(state, node_id, method, &params).await.map(|_| ())
}

/// 所有机器上的 Agent：本机实时扫描，远程机器用最近一次同步到的清单。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentEntry {
    pub node_id: String,
    pub node_name: String,
    /// 这台机器现在能不能用
    pub online: bool,
    #[serde(flatten)]
    pub agent: DiscoveredAgent,
}

#[tauri::command]
pub async fn agents_all(state: State<'_, Arc<AppState>>) -> Result<Vec<AgentEntry>, String> {
    let (tx, _rx) = mpsc::channel(4);
    let local: Vec<DiscoveredAgent> = serde_json::from_value(
        state
            .service
            .call("agents.list", &serde_json::json!({}), tx)
            .await?,
    )
    .map_err(|e| e.to_string())?;
    let mut list: Vec<AgentEntry> = local
        .into_iter()
        .map(|agent| AgentEntry {
            node_id: LOCAL.to_string(),
            node_name: LOCAL_NAME.to_string(),
            online: true,
            agent,
        })
        .collect();
    for view in state.remote.views() {
        list.extend(view.agents.into_iter().map(|agent| AgentEntry {
            node_id: view.id.clone(),
            node_name: view.name.clone(),
            online: view.online,
            agent,
        }));
    }
    Ok(list)
}

/// 机器页的一行
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineView {
    pub id: String,
    pub name: String,
    pub local: bool,
    pub online: bool,
    /// 机器信息：远程机器至少连上过一次才有
    pub info: Option<MachineInfo>,
    pub status: Option<crate::remote::NodeStatus>,
    pub agent_count: usize,
}

#[tauri::command]
pub async fn machines_list(state: State<'_, Arc<AppState>>) -> Result<Vec<MachineView>, String> {
    Ok(machines(&state).await)
}

pub async fn machines(state: &Arc<AppState>) -> Vec<MachineView> {
    let mut list = vec![MachineView {
        id: LOCAL.to_string(),
        name: LOCAL_NAME.to_string(),
        local: true,
        online: true,
        info: Some(local_info(state).await),
        status: None,
        agent_count: agents_on(state, LOCAL).await.len(),
    }];
    list.extend(state.remote.views().into_iter().map(|view| MachineView {
        local: false,
        agent_count: view.agents.len(),
        id: view.id,
        name: view.name,
        online: view.online,
        info: view.info,
        status: Some(view.status),
    }));
    list
}

async fn agents_on(state: &Arc<AppState>, node_id: &str) -> Vec<DiscoveredAgent> {
    match call(state, node_id, "agents.list", &serde_json::json!({})).await {
        Ok(value) => serde_json::from_value(value).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// 本机信息：第一次用到时取，之后沿用
async fn local_info(state: &Arc<AppState>) -> MachineInfo {
    if let Some(info) = state.local_info.lock().unwrap().clone() {
        return info;
    }
    let (tx, _rx) = mpsc::channel(4);
    let info = state
        .service
        .call("sys.info", &serde_json::json!({}), tx)
        .await
        .ok()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_else(|| MachineInfo {
            name: LOCAL_NAME.to_string(),
            hostname: String::new(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            kernel: None,
            version: env!("CARGO_PKG_VERSION").to_string(),
            flavor: "桌面应用内置".to_string(),
            home: String::new(),
            uptime_s: 0,
            cpu_count: 0,
            mem_total: 0,
        });
    *state.local_info.lock().unwrap() = Some(info.clone());
    info
}

/// 某台机器此刻的资源占用
#[tauri::command]
pub async fn node_metrics(
    state: State<'_, Arc<AppState>>,
    node_id: String,
) -> Result<Metrics, String> {
    let node = normalize(Some(&node_id));
    let value = call(&state, &node, "sys.metrics", &serde_json::json!({})).await?;
    serde_json::from_value(value).map_err(|e| format!("解析资源占用失败：{e}"))
}

/// 列出某台机器上的子目录（远程选择工作目录用）
#[tauri::command]
pub async fn node_ls(
    state: State<'_, Arc<AppState>>,
    node_id: String,
    path: String,
) -> Result<Value, String> {
    let node = normalize(Some(&node_id));
    call(&state, &node, "fs.ls", &serde_json::json!({ "path": path })).await
}
