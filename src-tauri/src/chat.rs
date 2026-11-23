//! 托管会话：以 headless 双向流驱动 CLI Agent（当前支持 claude-cli）。
//!
//! claude -p <prompt> --output-format stream-json --verbose [--resume <session_id>]
//! 每行一个 JSON 事件：assistant 增量文本 / result 终态（含 session_id 与费用）。
//! 事件经 Tauri event `chat-event` 推送到前端，消息落库。

use crate::AppState;
use rusqlite::params;
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use tokio::io::AsyncBufReadExt;

/// 目前实现了完整流式会话的 adapter；其余 CLI 将逐步适配。
const SUPPORTED_ADAPTERS: &[&str] = &["claude-cli"];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSession {
    pub id: i64,
    pub adapter: String,
    pub title: String,
    pub created_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: i64,
    pub session_id: i64,
    pub role: String,
    pub content: String,
    pub cost_usd: Option<f64>,
    pub created_at: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum ChatEvent {
    #[serde(rename = "delta")]
    Delta { session_id: i64, text: String },
    #[serde(rename = "done")]
    Done { session_id: i64, cost_usd: Option<f64> },
    #[serde(rename = "error")]
    Error { session_id: i64, message: String },
}

fn claude_binary() -> String {
    let dirs = crate::discovery::search_dirs();
    crate::discovery::find_binary("claude", &dirs)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "claude".into())
}

fn insert_message(state: &AppState, session_id: i64, role: &str, content: &str, cost: Option<f64>) {
    let db = state.db.lock().unwrap();
    let _ = db.execute(
        "INSERT INTO messages (session_id, role, content, cost_usd) VALUES (?1, ?2, ?3, ?4)",
        params![session_id, role, content, cost],
    );
}

fn emit(app: &AppHandle, event: ChatEvent) {
    let _ = app.emit("chat-event", event);
}

/// 执行一轮对话：spawn claude 进程，流式转发到前端，结束落库。
async fn run_turn(app: AppHandle, state: Arc<AppState>, session_id: i64, resume: Option<String>, prompt: String) {
    let bin = claude_binary();
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.arg("-p")
        .arg(&prompt)
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose");
    if let Some(r) = &resume {
        cmd.arg("--resume").arg(r);
    }
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("启动 {bin} 失败: {e}（可在 Agents 页确认该 Agent 已安装）");
            insert_message(&state, session_id, "system", &msg, None);
            emit(&app, ChatEvent::Error { session_id, message: msg });
            return;
        }
    };

    // 排空 stderr，避免缓冲区写满阻塞子进程
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(_)) = lines.next_line().await {}
        });
    }

    let stdout = match child.stdout.take() {
        Some(o) => o,
        None => {
            emit(&app, ChatEvent::Error { session_id, message: "无法读取输出流".into() });
            return;
        }
    };

    let mut acc = String::new();
    let mut session_ref: Option<String> = None;
    let mut cost: Option<f64> = None;
    let mut result_text: Option<String> = None;
    let mut lines = tokio::io::BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        match v["type"].as_str() {
            // assistant 消息：content 是块数组，取 text 块
            Some("assistant") => {
                if let Some(blocks) = v["message"]["content"].as_array() {
                    for b in blocks {
                        if b["type"].as_str() == Some("text") {
                            if let Some(t) = b["text"].as_str() {
                                acc.push_str(t);
                                emit(&app, ChatEvent::Delta { session_id, text: t.to_string() });
                            }
                        }
                    }
                }
            }
            // 终态：session_id 供 --resume 续聊，total_cost_usd 供成本统计
            Some("result") => {
                session_ref = v["session_id"].as_str().map(|x| x.to_string());
                cost = v["total_cost_usd"].as_f64();
                result_text = v["result"].as_str().map(|x| x.to_string());
            }
            _ => {}
        }
    }
    let _ = child.wait().await;

    let final_text = if acc.is_empty() {
        result_text.clone().unwrap_or_default()
    } else {
        acc
    };
    if final_text.is_empty() {
        let msg = "Agent 无输出（进程可能异常退出，或未配置 API Key）".to_string();
        insert_message(&state, session_id, "system", &msg, None);
        emit(&app, ChatEvent::Error { session_id, message: msg });
        return;
    }
    insert_message(&state, session_id, "assistant", &final_text, cost);
    if let Some(r) = &session_ref {
        let db = state.db.lock().unwrap();
        let _ = db.execute(
            "UPDATE sessions SET external_ref = ?1 WHERE id = ?2 AND (external_ref IS NULL OR external_ref = '')",
            params![r, session_id],
        );
    }
    emit(&app, ChatEvent::Done { session_id, cost_usd: cost });
}

#[tauri::command]
pub fn chat_sessions(state: State<'_, Arc<AppState>>) -> Result<Vec<ChatSession>, String> {
    let db = state.db.lock().unwrap();
    let mut stmt = db
        .prepare("SELECT id, adapter, title, created_at FROM sessions ORDER BY id DESC LIMIT 100")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ChatSession {
                id: row.get(0)?,
                adapter: row.get(1)?,
                title: row.get(2)?,
                created_at: row.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

#[tauri::command]
pub fn chat_messages(
    state: State<'_, Arc<AppState>>,
    session_id: i64,
) -> Result<Vec<ChatMessage>, String> {
    let db = state.db.lock().unwrap();
    let mut stmt = db
        .prepare("SELECT id, session_id, role, content, cost_usd, created_at FROM messages WHERE session_id = ?1 ORDER BY id")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![session_id], |row| {
            Ok(ChatMessage {
                id: row.get(0)?,
                session_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                cost_usd: row.get(4)?,
                created_at: row.get(5)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

/// 开启新会话并发送第一条消息。
#[tauri::command]
pub async fn chat_start(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    adapter: String,
    prompt: String,
) -> Result<i64, String> {
    if !SUPPORTED_ADAPTERS.contains(&adapter.as_str()) {
        return Err(format!(
            "「{adapter}」的会话适配将在后续版本开放，当前支持: claude-cli"
        ));
    }
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err("消息不能为空".into());
    }
    let title: String = prompt.chars().take(24).collect();
    let sid = {
        let db = state.db.lock().unwrap();
        db.execute(
            "INSERT INTO sessions (adapter, title) VALUES (?1, ?2)",
            params![adapter, title],
        )
        .map_err(|e| e.to_string())?;
        let id = db.last_insert_rowid();
        db.execute(
            "INSERT INTO messages (session_id, role, content) VALUES (?1, 'user', ?2)",
            params![id, prompt],
        )
        .map_err(|e| e.to_string())?;
        id
    };
    let st = state.inner().clone();
    tauri::async_runtime::spawn(async move {
        run_turn(app, st, sid, None, prompt).await;
    });
    Ok(sid)
}

/// 在已有会话中续聊（claude 通过 --resume 保持上下文）。
#[tauri::command]
pub async fn chat_send(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    session_id: i64,
    prompt: String,
) -> Result<(), String> {
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err("消息不能为空".into());
    }
    let (adapter, resume) = {
        let db = state.db.lock().unwrap();
        let row = db
            .query_row(
                "SELECT adapter, external_ref FROM sessions WHERE id = ?1",
                params![session_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .map_err(|e| format!("会话不存在: {e}"))?;
        row
    };
    if !SUPPORTED_ADAPTERS.contains(&adapter.as_str()) {
        return Err("该会话的 Agent 暂不支持续聊".into());
    }
    {
        let db = state.db.lock().unwrap();
        db.execute(
            "INSERT INTO messages (session_id, role, content) VALUES (?1, 'user', ?2)",
            params![session_id, prompt],
        )
        .map_err(|e| e.to_string())?;
    }
    let st = state.inner().clone();
    tauri::async_runtime::spawn(async move {
        run_turn(app, st, session_id, resume, prompt).await;
    });
    Ok(())
}
