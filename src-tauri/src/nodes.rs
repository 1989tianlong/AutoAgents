//! 节点：控制端对「某台机器」的统一入口。
//!
//! 本机是一个内嵌的节点（`local`，机器侧服务就在应用进程里）；
//! 远程机器由 agentd 提供同一个服务，请求经加密通道转发过去。
//! 上层模块只认「节点 id + 请求」，不关心它在本地还是远程。

use crate::chat::NodeEvent;
use crate::AppState;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;

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
        // 远程节点：由 nodes::remote 接入（见下）
        crate::nodes_remote::call(state, node_id, method, params).await
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
    let method = if workdir.is_some() { "fs.ls" } else { "sys.info" };
    call(state, node_id, method, &params).await.map(|_| ())
}
