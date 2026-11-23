//! 远程节点：经中继 / 直连的加密通道把请求转发给 agentd。
//! （多机接入的实现见本模块后续扩展；当前先占位，保证本机路径可用。）

use crate::AppState;
use serde_json::Value;
use std::sync::Arc;

pub async fn call(
    _state: &Arc<AppState>,
    node_id: &str,
    _method: &str,
    _params: &Value,
) -> Result<Value, String> {
    Err(format!("机器「{node_id}」不在线"))
}
