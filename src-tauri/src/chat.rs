//! 托管会话（控制端）：把一轮对话发给某台机器上的 Agent，流式输出到前端并落库。
//!
//! 本机与远程节点走同一条路：请求经 [`crate::nodes`] 送到机器侧的服务，
//! 过程事件汇入事件总线，由 [`dispatch`] 按会话归档：
//! 推送 `chat-event` → 留一份实时快照 → 结束后落库。

use autoagents_core::adapters;
use autoagents_core::runner::{TurnEvent, TurnParams};
use crate::nodes;
use crate::AppState;
use rusqlite::params;
use serde::Serialize;
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::mpsc;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSession {
    pub id: i64,
    pub adapter: String,
    pub title: String,
    pub model: Option<String>,
    pub workdir: Option<String>,
    /// 会话所在机器的节点 id
    pub node_id: String,
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
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    /// 这一轮从发出到结束的耗时
    pub duration_ms: Option<i64>,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum ChatEvent {
    #[serde(rename = "delta")]
    Delta {
        session_id: i64,
        text: String,
    },
    /// 过程提示（工具调用、思考等），不落库
    #[serde(rename = "notice")]
    Notice {
        session_id: i64,
        text: String,
    },
    /// 本轮结束时的完整文本，前端用它校正流式预览
    #[serde(rename = "replace")]
    Replace {
        session_id: i64,
        text: String,
    },
    #[serde(rename = "done")]
    Done {
        session_id: i64,
        cost_usd: Option<f64>,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
    },
    #[serde(rename = "error")]
    Error {
        session_id: i64,
        message: String,
    },
}

impl ChatEvent {
    pub fn session_id(&self) -> i64 {
        match self {
            ChatEvent::Delta { session_id, .. }
            | ChatEvent::Notice { session_id, .. }
            | ChatEvent::Replace { session_id, .. }
            | ChatEvent::Done { session_id, .. }
            | ChatEvent::Error { session_id, .. } => *session_id,
        }
    }
}

/// 把事件送到界面。运行时是 Tauri 的事件推送，测试里换成收集器。
pub trait Sink: Send + Sync + 'static {
    fn send(&self, event: ChatEvent);
}

/// Tauri 的事件推送：所有窗口都会收到
pub struct TauriSink(pub AppHandle);

impl Sink for TauriSink {
    fn send(&self, event: ChatEvent) {
        if let Err(e) = self.0.emit("chat-event", event) {
            eprintln!("[chat] 推送事件失败：{e}");
        }
    }
}

/// 事件总线上的一则对话事件：来自哪台机器、内容是什么
pub struct NodeEvent {
    pub node: String,
    pub event: TurnEvent,
}

/// 一轮对话的输入
struct Turn {
    adapter: String,
    resume: Option<String>,
    model: Option<String>,
    /// 机器上的路径，原样回传（可能是 `~` 开头）
    workdir: String,
    prompt: String,
}

fn turn_params(session_id: i64, turn: &Turn) -> TurnParams {
    TurnParams {
        turn_id: session_id.to_string(),
        adapter: turn.adapter.clone(),
        model: turn.model.clone(),
        resume: turn.resume.clone(),
        workdir: Some(turn.workdir.clone()),
        prompt: turn.prompt.clone(),
    }
}

#[derive(Default, Clone, Copy)]
struct Usage {
    cost_usd: Option<f64>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
}

fn insert_message(
    state: &AppState,
    session_id: i64,
    role: &str,
    content: &str,
    usage: Usage,
    duration_ms: Option<i64>,
) {
    let db = state.db.lock().unwrap();
    let _ = db.execute(
        "INSERT INTO messages (session_id, role, content, cost_usd, input_tokens, output_tokens, duration_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            session_id,
            role,
            content,
            usage.cost_usd,
            usage.input_tokens,
            usage.output_tokens,
            duration_ms
        ],
    );
}

fn emit(state: &AppState, session_id: i64, event: ChatEvent) {
    // 同时留一份在内存里：窗口不可见时推送事件可能没有送达，界面会据此校正状态
    if let Ok(mut live) = state.live.lock() {
        if let Some(turn) = live.get_mut(&session_id) {
            match &event {
                ChatEvent::Delta { text, .. } => turn.text.push_str(text),
                ChatEvent::Notice { text, .. } => turn.activity = Some(text.clone()),
                ChatEvent::Replace { text, .. } => turn.text = text.clone(),
                _ => {}
            }
        }
    }
    state.sink.send(event);
}

/// 本轮以失败收尾：写入系统消息、通知前端、从运行中摘掉
fn fail(state: &AppState, session_id: i64, message: &str) {
    insert_message(state, session_id, "system", message, Usage::default(), None);
    emit(
        state,
        session_id,
        ChatEvent::Error {
            session_id,
            message: message.to_string(),
        },
    );
    state.live.lock().unwrap().remove(&session_id);
}

/// 首次拿到外部会话标识就落库：应用中途退出后，会话仍然可以续聊
fn remember_ref(state: &AppState, session_id: i64, external_ref: &str) {
    let db = state.db.lock().unwrap();
    let _ = db.execute(
        "UPDATE sessions SET external_ref = ?1 WHERE id = ?2 AND (external_ref IS NULL OR external_ref = '')",
        params![external_ref, session_id],
    );
}

/// 归档一则对话事件：只处理仍在运行、且正是这台机器上那一轮的事件
/// （断线重连后，旧连接迟到的事件会被丢弃）。
fn handle(state: &Arc<AppState>, session_id: i64, node: &str, event: TurnEvent) {
    let started = {
        let live = state.live.lock().unwrap();
        match live.get(&session_id) {
            Some(turn) if turn.node == node => turn.started,
            _ => return,
        }
    };
    match event {
        TurnEvent::Session { r#ref, .. } => remember_ref(state, session_id, &r#ref),
        TurnEvent::Delta { text, .. } => {
            emit(state, session_id, ChatEvent::Delta { session_id, text })
        }
        TurnEvent::Notice { text, .. } => {
            emit(state, session_id, ChatEvent::Notice { session_id, text })
        }
        TurnEvent::System { message, .. } => {
            insert_message(state, session_id, "system", &message, Usage::default(), None)
        }
        TurnEvent::Failed { message, .. } => fail(state, session_id, &message),
        TurnEvent::Finished {
            text,
            cost_usd,
            input_tokens,
            output_tokens,
            ..
        } => {
            let usage = Usage {
                cost_usd,
                input_tokens,
                output_tokens,
            };
            insert_message(
                state,
                session_id,
                "assistant",
                &text,
                usage,
                Some(started.elapsed().as_millis() as i64),
            );
            emit(
                state,
                session_id,
                ChatEvent::Replace {
                    session_id,
                    text: text.clone(),
                },
            );
            emit(
                state,
                session_id,
                ChatEvent::Done {
                    session_id,
                    cost_usd: usage.cost_usd,
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                },
            );
            state.live.lock().unwrap().remove(&session_id);
        }
    }
}

/// 事件总线 → 会话归档。应用启动时启动一个，常驻。
pub fn dispatch(state: Arc<AppState>, mut rx: mpsc::UnboundedReceiver<NodeEvent>) {
    tauri::async_runtime::spawn(async move {
        while let Some(NodeEvent { node, event }) = rx.recv().await {
            let Ok(session_id) = event.turn_id().parse::<i64>() else {
                continue;
            };
            handle(&state, session_id, &node, event);
        }
    });
}

/// 登记为运行中并发出这一轮。先登记再启动：此后任何一次状态查询都能看到这个会话，
/// 事件总线上的事件也都有地方归档。之后的收尾由事件总线上的终态事件触发。
fn spawn_turn(state: &Arc<AppState>, session_id: i64, turn: Turn, node: String) {
    state.live.lock().unwrap().insert(
        session_id,
        LiveTurn {
            node: node.clone(),
            ..Default::default()
        },
    );
    let state = Arc::clone(state);
    tauri::async_runtime::spawn(async move {
        let params = serde_json::to_value(turn_params(session_id, &turn)).expect("序列化对话参数");
        // 没能启动的（机器掉线、目录被删……）：这一轮以失败收尾
        if let Err(e) = nodes::call(&state, &node, "turn.start", &params).await {
            fail(&state, session_id, &e);
        }
    });
}

/// 开启新会话并发送第一条消息。model 为空时沿用 Agent 自身配置的默认模型；
/// node_id 为空时在本机执行。
pub async fn start(
    state: &Arc<AppState>,
    node_id: Option<&str>,
    adapter: &str,
    model: Option<&str>,
    workdir: Option<&str>,
    prompt: &str,
) -> Result<i64, String> {
    if !adapters::is_supported(adapter) {
        return Err(format!("「{adapter}」的会话适配尚未实现"));
    }
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err("消息不能为空".into());
    }
    let model = model
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty());
    let workdir = workdir
        .map(|w| w.trim().to_string())
        .filter(|w| !w.is_empty());
    let node = nodes::normalize(node_id);
    // 机器在线、目录存在才建会话，免得留下一开口就失败的记录
    nodes::ensure_ready(state, &node, workdir.as_deref()).await?;
    // 工作目录留空时由机器侧取它的主目录（`~` 指的是那台机器上的主目录）
    let workdir = workdir.unwrap_or_else(|| "~".to_string());
    let title: String = prompt.chars().take(24).collect();
    let sid = {
        let db = state.db.lock().unwrap();
        db.execute(
            "INSERT INTO sessions (adapter, title, model, workdir, node_id) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![adapter, title, model, workdir, node],
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
    let turn = Turn {
        adapter: adapter.to_string(),
        resume: None,
        model,
        workdir,
        prompt,
    };
    spawn_turn(state, sid, turn, node);
    Ok(sid)
}

/// 在已有会话中续聊：沿用该会话的机器、Agent、模型与工作目录。
pub async fn send(state: &Arc<AppState>, session_id: i64, prompt: &str) -> Result<(), String> {
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err("消息不能为空".into());
    }
    let (adapter, resume, model, workdir, node) = {
        let db = state.db.lock().unwrap();
        db.query_row(
            "SELECT adapter, external_ref, model, workdir, node_id FROM sessions WHERE id = ?1",
            params![session_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .map_err(|_| format!("会话不存在：#{session_id}"))?
    };
    if !adapters::is_supported(&adapter) {
        return Err("该会话的 Agent 暂不支持续聊".into());
    }
    let node = nodes::normalize(node.as_deref());
    // 升级前创建的会话没有记录工作目录，留空让机器侧取它的主目录
    let workdir = workdir.unwrap_or_default();
    // 同一会话同一时间只跑一轮：先登记占住，后面任何一步失败再摘掉
    {
        let mut live = state.live.lock().unwrap();
        if live.contains_key(&session_id) {
            return Err("该会话上一轮还在运行，请等它结束或先停止".into());
        }
        live.insert(
            session_id,
            LiveTurn {
                node: node.clone(),
                ..Default::default()
            },
        );
    }
    let undo = |state: &AppState| state.live.lock().unwrap().remove(&session_id);
    if let Err(e) = nodes::ensure_ready(state, &node, Some(&workdir)).await {
        undo(state);
        return Err(e);
    }
    {
        let db = state.db.lock().unwrap();
        if let Err(e) = db.execute(
            "INSERT INTO messages (session_id, role, content) VALUES (?1, 'user', ?2)",
            params![session_id, prompt],
        ) {
            drop(db);
            undo(state);
            return Err(e.to_string());
        }
    }
    let turn = Turn {
        adapter,
        resume,
        model,
        workdir,
        prompt,
    };
    // 已经登记过；这里只负责发出
    let st = Arc::clone(state);
    tauri::async_runtime::spawn(async move {
        let params = serde_json::to_value(turn_params(session_id, &turn)).expect("序列化对话参数");
        if let Err(e) = nodes::call(&st, &node, "turn.start", &params).await {
            fail(&st, session_id, &e);
        }
    });
    Ok(())
}

#[tauri::command]
pub async fn chat_start(
    state: State<'_, Arc<AppState>>,
    node_id: Option<String>,
    adapter: String,
    model: Option<String>,
    workdir: Option<String>,
    prompt: String,
) -> Result<i64, String> {
    start(
        &state,
        node_id.as_deref(),
        &adapter,
        model.as_deref(),
        workdir.as_deref(),
        &prompt,
    )
    .await
}

#[tauri::command]
pub async fn chat_send(
    state: State<'_, Arc<AppState>>,
    session_id: i64,
    prompt: String,
) -> Result<(), String> {
    send(&state, session_id, &prompt).await
}

#[tauri::command]
pub fn chat_sessions(state: State<'_, Arc<AppState>>) -> Result<Vec<ChatSession>, String> {
    let db = state.db.lock().unwrap();
    let mut stmt = db
        .prepare(
            "SELECT id, adapter, title, model, workdir, node_id, created_at FROM sessions ORDER BY id DESC LIMIT 100",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ChatSession {
                id: row.get(0)?,
                adapter: row.get(1)?,
                title: row.get(2)?,
                model: row.get(3)?,
                workdir: row.get(4)?,
                node_id: row
                    .get::<_, Option<String>>(5)?
                    .unwrap_or_else(|| nodes::LOCAL.to_string()),
                created_at: row.get(6)?,
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
        .prepare(
            "SELECT id, session_id, role, content, cost_usd, input_tokens, output_tokens, duration_ms, created_at
             FROM messages WHERE session_id = ?1 ORDER BY id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![session_id], |row| {
            Ok(ChatMessage {
                id: row.get(0)?,
                session_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                cost_usd: row.get(4)?,
                input_tokens: row.get(5)?,
                output_tokens: row.get(6)?,
                duration_ms: row.get(7)?,
                created_at: row.get(8)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

/// 运行中的一轮对话目前的界面内容。
pub struct LiveTurn {
    text: String,
    activity: Option<String>,
    node: String,
    started: Instant,
}

impl Default for LiveTurn {
    fn default() -> Self {
        Self {
            text: String::new(),
            activity: None,
            node: String::new(),
            started: Instant::now(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveTurnInfo {
    pub session_id: i64,
    pub text: String,
    pub activity: Option<String>,
    pub node_id: String,
}

/// 正在运行的会话及其实时输出。界面以它为准：窗口不可见时推送事件可能没有送达，
/// 重新可见（或定时）时据此补登记、补收尾。
pub fn live(state: &Arc<AppState>) -> Vec<LiveTurnInfo> {
    let live = state.live.lock().unwrap();
    let mut list: Vec<LiveTurnInfo> = live
        .iter()
        .map(|(id, turn)| LiveTurnInfo {
            session_id: *id,
            text: turn.text.clone(),
            activity: turn.activity.clone(),
            node_id: turn.node.clone(),
        })
        .collect();
    list.sort_by_key(|turn| turn.session_id);
    list
}

/// 停止正在运行的一轮对话；已产出的文本会保留。
pub async fn stop(state: &Arc<AppState>, session_id: i64) -> Result<(), String> {
    let node = {
        let live = state.live.lock().unwrap();
        live.get(&session_id)
            .map(|turn| turn.node.clone())
            .ok_or_else(|| format!("会话 #{session_id} 没有在运行"))?
    };
    nodes::call(
        &state,
        &node,
        "turn.stop",
        &serde_json::json!({ "turnId": session_id.to_string() }),
    )
    .await?;
    Ok(())
}

#[tauri::command]
pub fn chat_live(state: State<'_, Arc<AppState>>) -> Vec<LiveTurnInfo> {
    live(&state)
}

#[tauri::command]
pub async fn chat_stop(state: State<'_, Arc<AppState>>, session_id: i64) -> Result<(), String> {
    stop(&state, session_id).await
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use autoagents_core::runner::BinaryResolver;
    use autoagents_core::service::{Service, ServiceOptions};
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::Duration;

    /// 收集推给界面的事件
    #[derive(Clone, Default)]
    struct Collector(Arc<Mutex<Vec<ChatEvent>>>);

    impl Sink for Collector {
        fn send(&self, event: ChatEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    /// 造一个冒充 claude 的脚本，并返回只认它的服务
    fn fake_agent(script: &str) -> (PathBuf, Arc<Service>) {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        std::fs::write(&bin, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        std::mem::forget(dir);
        let fake = bin.clone();
        let service = Service::new(ServiceOptions {
            resolve: Some(Arc::new(move |_: &str| Some(fake.clone())) as BinaryResolver),
            ..Default::default()
        });
        (bin, service)
    }

    fn test_state(service: Arc<Service>) -> (Arc<AppState>, Collector) {
        let collector = Collector::default();
        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        let state = AppState::new(db, service, Arc::new(collector.clone()));
        (state, collector)
    }

    type Row = (String, String, Option<f64>, Option<i64>);

    fn rows(state: &AppState, session_id: i64) -> Vec<Row> {
        let db = state.db.lock().unwrap();
        let mut stmt = db
            .prepare("SELECT role, content, cost_usd, duration_ms FROM messages WHERE session_id = ?1 ORDER BY id")
            .unwrap();
        stmt.query_map(params![session_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    }

    /// 等这一轮从运行中摘掉
    async fn settle(state: &AppState, session_id: i64) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while state.live.lock().unwrap().contains_key(&session_id) {
            assert!(Instant::now() < deadline, "会话 #{session_id} 一直停在运行中");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    const OK: &str = r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"s1"}' '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"hel"}}}' '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"lo"}}}' '{"type":"result","is_error":false,"result":"hello","total_cost_usd":0.02,"usage":{"input_tokens":5,"output_tokens":2}}'"#;

    /// 完整一轮：用户消息落库 → 事件推给界面 → 回答带用量与耗时落库 → 会话标识记住
    #[tokio::test]
    async fn turn_streams_persists_and_settles() {
        let (_, service) = fake_agent(OK);
        let (state, collector) = test_state(service);
        let id = start(&state, None, "claude-cli", Some("glm-5.3"), Some("~"), "hi").await.unwrap();
        settle(&state, id).await;
        let rows = rows(&state, id);
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].0.as_str(), rows[0].1.as_str()), ("user", "hi"));
        assert_eq!((rows[1].0.as_str(), rows[1].1.as_str()), ("assistant", "hello"));
        assert_eq!(rows[1].2, Some(0.02));
        assert!(rows[1].3.is_some(), "回答要带耗时");

        let events: Vec<ChatEvent> = collector.0.lock().unwrap().iter().cloned().collect();
        let kinds: Vec<String> = events
            .iter()
            .map(|e| serde_json::to_value(e).unwrap()["type"].as_str().unwrap().to_string())
            .collect();
        // 增量只推给界面，不落库；结束时用完整文本校正
        assert_eq!(kinds, ["delta", "delta", "replace", "done"]);
        assert!(events.iter().all(|e| e.session_id() == id));
        let ChatEvent::Done { cost_usd, input_tokens, output_tokens, .. } = &events[3] else {
            panic!("最后一则应当是 Done：{events:?}");
        };
        assert_eq!((*cost_usd, *input_tokens, *output_tokens), (Some(0.02), Some(5), Some(2)));

        let db = state.db.lock().unwrap();
        let (r#ref, node, model): (String, String, String) = db
            .query_row(
                "SELECT external_ref, node_id, model FROM sessions WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!((r#ref.as_str(), node.as_str(), model.as_str()), ("s1", "local", "glm-5.3"));
    }

    /// 窗口不可见、事件全丢：靠 live 快照也能看到进度，结束后快照清空
    #[tokio::test]
    async fn live_snapshot_tracks_progress() {
        let (_, service) = fake_agent(OK);
        let (state, _) = test_state(service);
        let id = start(&state, None, "claude-cli", None, None, "hi").await.unwrap();
        settle(&state, id).await;
        let running = live(&state);
        assert!(running.is_empty(), "结束后不应再有运行中的会话：{running:?}");
    }

    #[tokio::test]
    async fn start_rejects_bad_input_without_creating_a_session() {
        let (bin, service) = fake_agent(OK);
        let (state, _) = test_state(service);
        assert!(start(&state, None, "zcode", None, None, "hi").await.unwrap_err().contains("适配"));
        assert!(start(&state, None, "claude-cli", None, None, "  ").await.unwrap_err().contains("不能为空"));
        let missing = bin.parent().unwrap().join("no-such-dir");
        let err = start(&state, None, "claude-cli", None, Some(&missing.to_string_lossy()), "hi")
            .await
            .unwrap_err();
        assert!(err.contains("工作目录不存在"), "{err}");
        assert!(start(&state, Some("nope"), "claude-cli", None, None, "hi")
            .await
            .unwrap_err()
            .contains("不在线"));
        let db = state.db.lock().unwrap();
        let count: i64 = db.query_row("SELECT count(*) FROM sessions", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0, "校验失败不应留下会话记录");
    }

    /// 续聊带上外部会话标识与模型；同一会话不能同时跑两轮
    #[tokio::test]
    async fn send_resumes_and_is_exclusive() {
        // 把收到的参数原样当作回答带回来，便于断言命令行
        let echo = r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"s1"}' "{\"type\":\"result\",\"is_error\":false,\"result\":\"$*\"}""#;
        let (_, service) = fake_agent(echo);
        let (state, _) = test_state(service);
        let id = start(&state, None, "claude-cli", Some("glm-5.3"), None, "one").await.unwrap();
        settle(&state, id).await;
        assert!(send(&state, id, "two").await.is_ok());
        settle(&state, id).await;
        let history = rows(&state, id);
        assert_eq!(history.len(), 4, "两轮各一条用户消息、一条回答");
        assert!(history[1].1.contains("--model glm-5.3"), "{}", history[1].1);
        assert!(!history[1].1.contains("--resume"), "首轮不应续聊：{}", history[1].1);
        assert!(history[3].1.contains("--resume s1"), "续聊要带上会话标识：{}", history[3].1);
        assert!(history[3].1.contains("--model glm-5.3"), "{}", history[3].1);
        assert!(send(&state, 9999, "x").await.unwrap_err().contains("不存在"));
        assert!(send(&state, id, " ").await.unwrap_err().contains("不能为空"));

        // 运行中再发：拒绝，且不落用户消息
        let (_, slow) = fake_agent("sleep 30");
        let (state, _) = test_state(slow);
        let id = start(&state, None, "claude-cli", None, None, "slow").await.unwrap();
        assert!(state.live.lock().unwrap().contains_key(&id));
        assert!(send(&state, id, "again").await.unwrap_err().contains("还在运行"));
        assert_eq!(rows(&state, id).len(), 1);
        stop(&state, id).await.unwrap();
        settle(&state, id).await;
        assert!(stop(&state, id).await.unwrap_err().contains("没有在运行"));
    }

    /// 中途停止：已经流出来的文本保留为回答
    #[tokio::test]
    async fn stop_keeps_partial_output() {
        let partial = r#"printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"部分"}}}'
sleep 30"#;
        let (_, service) = fake_agent(partial);
        let (state, collector) = test_state(service);
        let id = start(&state, None, "claude-cli", None, None, "hi").await.unwrap();
        // 等第一个增量到达再停
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if state.live.lock().unwrap().get(&id).is_some_and(|t| t.text == "部分") {
                break;
            }
            assert!(Instant::now() < deadline, "没有收到增量");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        stop(&state, id).await.unwrap();
        settle(&state, id).await;
        let history = rows(&state, id);
        assert_eq!(history.len(), 3, "{history:?}");
        assert_eq!((history[1].0.as_str(), history[1].1.as_str()), ("system", "已停止：完整回答尚未生成，以上是已经产出的内容"));
        assert_eq!((history[2].0.as_str(), history[2].1.as_str()), ("assistant", "部分"));
        let events = collector.0.lock().unwrap();
        assert!(events.iter().any(|e| matches!(e, ChatEvent::Done { .. })));
        assert!(!events.iter().any(|e| matches!(e, ChatEvent::Error { .. })));
    }

    /// Agent 启动不了：错误以系统消息出现，会话不卡在运行中
    #[tokio::test]
    async fn launch_failure_is_reported() {
        let service = Service::new(ServiceOptions {
            resolve: Some(Arc::new(|_: &str| None) as BinaryResolver),
            ..Default::default()
        });
        let (state, collector) = test_state(service);
        let id = start(&state, None, "codex-cli", None, None, "hi").await.unwrap();
        settle(&state, id).await;
        let rows = rows(&state, id);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].0, "system");
        assert!(rows[1].1.contains("未找到"), "{}", rows[1].1);
        assert!(matches!(
            collector.0.lock().unwrap().last(),
            Some(ChatEvent::Error { session_id, .. }) if *session_id == id
        ));
    }

    /// 不是这台机器上那一轮的事件、已结束会话的迟到事件：一律忽略
    #[tokio::test]
    async fn stale_events_are_ignored() {
        let (_, service) = fake_agent("sleep 30");
        let (state, _) = test_state(service);
        let id = start(&state, None, "claude-cli", None, None, "hi").await.unwrap();
        let stale = TurnEvent::Finished {
            turn_id: "424242".into(),
            text: "迟到的".into(),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            incomplete: false,
        };
        let _ = state.events.send(NodeEvent {
            node: "local".into(),
            event: stale,
        });
        // 来源机器对不上的增量也不采纳
        let _ = state.events.send(NodeEvent {
            node: "other-machine".into(),
            event: TurnEvent::Delta {
                turn_id: id.to_string(),
                text: "串线".into(),
            },
        });
        let _ = state.events.send(NodeEvent {
            node: "local".into(),
            event: TurnEvent::Delta {
                turn_id: id.to_string(),
                text: "本机".into(),
            },
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while state.live.lock().unwrap().get(&id).is_some_and(|t| t.text != "本机") {
            assert!(Instant::now() < deadline, "本机的增量应当被采纳");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        stop(&state, id).await.unwrap();
        settle(&state, id).await;
        let db = state.db.lock().unwrap();
        let count: i64 = db
            .query_row("SELECT count(*) FROM sessions WHERE id = 424242", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
