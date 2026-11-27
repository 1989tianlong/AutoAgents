//! 模型清单：Tauri 命令层。清单在哪台机器上执行就读哪台机器的配置，
//! 控制端只负责记住用户手动填写过的模型。

use crate::AppState;
use crate::nodes;
use autoagents_core::models::{self as core, ModelList};
use rusqlite::params;
use std::sync::Arc;
use tauri::State;

/// 某个 Agent 在指定机器上可选的模型：机器侧读到的清单 + 用户手动填写过的模型。
#[tauri::command]
pub async fn agent_models(
    state: State<'_, Arc<AppState>>,
    node_id: Option<String>,
    adapter: String,
) -> Result<ModelList, String> {
    let node = nodes::normalize(node_id.as_deref());
    let mut list: ModelList = if nodes::is_local(&node) {
        core::load_models(&adapter).await?
    } else {
        serde_json::from_value(
            nodes::call(
                &state,
                &node,
                "models.list",
                &serde_json::json!({ "adapter": adapter }),
            )
            .await?,
        )
        .map_err(|e| format!("解析模型清单失败：{e}"))?
    };
    let custom = {
        let db = state.db.lock().unwrap();
        let mut stmt = db
            .prepare(
                "SELECT model FROM custom_models WHERE adapter = ?1 ORDER BY created_at DESC, model",
            )
            .map_err(|e| e.to_string())?;
        stmt.query_map(params![adapter], |row| row.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<String>, _>>()
            .map_err(|e| e.to_string())?
    };
    list.custom = custom;
    Ok(list)
}

/// 记住用户手动填写的模型，下次出现在该 Agent 的下拉里。
#[tauri::command]
pub fn model_remember(
    state: State<'_, Arc<AppState>>,
    adapter: String,
    model: String,
) -> Result<(), String> {
    let model = model.trim();
    if model.is_empty() {
        return Err("模型 ID 不能为空".into());
    }
    let db = state.db.lock().unwrap();
    db.execute(
        "INSERT OR IGNORE INTO custom_models (adapter, model) VALUES (?1, ?2)",
        params![adapter, model],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}
