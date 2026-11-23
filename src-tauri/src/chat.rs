//! 托管会话：以无头模式驱动 CLI Agent，流式输出到前端并落库。
//!
//! 各 Agent 的命令与协议差异见 `adapters`；一轮对话的流程是统一的：
//! 拉起子进程 → 逐行解析 → 推送 `chat-event` → 结束后落库。

use crate::adapters::{self, TurnEvent, TurnState, TurnSpec};
use crate::AppState;
use rusqlite::params;
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};
use tokio::io::{AsyncBufReadExt, BufReader};

/// 进程异常退出时，附在错误信息里的 stderr 长度上限（字符数）
const STDERR_TAIL_CHARS: usize = 600;

/// 进程自行退出（保存会话、关闭拉起的子进程）的等待时间，超过就连同进程组一起结束
const EXIT_GRACE: Duration = Duration::from_secs(5);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSession {
    pub id: i64,
    pub adapter: String,
    pub title: String,
    pub model: Option<String>,
    pub workdir: Option<String>,
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
    pub created_at: String,
}

#[derive(Clone, Serialize)]
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

/// 一轮对话的全部输入
struct Turn {
    session_id: i64,
    adapter: String,
    resume: Option<String>,
    model: Option<String>,
    workdir: PathBuf,
    prompt: String,
}

fn insert_message(
    state: &AppState,
    session_id: i64,
    role: &str,
    content: &str,
    cost_usd: Option<f64>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
) {
    let db = state.db.lock().unwrap();
    let _ = db.execute(
        "INSERT INTO messages (session_id, role, content, cost_usd, input_tokens, output_tokens)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![session_id, role, content, cost_usd, input_tokens, output_tokens],
    );
}

fn emit(app: &AppHandle, state: &AppState, session_id: i64, event: ChatEvent) {
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
    if let Err(e) = app.emit("chat-event", event) {
        eprintln!("[chat] 推送事件失败：{e}");
    }
}

/// 本轮失败：写入系统消息并通知前端
fn fail(app: &AppHandle, state: &AppState, session_id: i64, message: &str) {
    insert_message(state, session_id, "system", message, None, None, None);
    emit(
        app,
        state,
        session_id,
        ChatEvent::Error {
            session_id,
            message: message.to_string(),
        },
    );
}

/// 首次拿到外部会话标识就落库：应用中途退出后，会话仍然可以续聊
fn remember_ref(state: &AppState, session_id: i64, external_ref: &str) {
    let db = state.db.lock().unwrap();
    let _ = db.execute(
        "UPDATE sessions SET external_ref = ?1 WHERE id = ?2 AND (external_ref IS NULL OR external_ref = '')",
        params![external_ref, session_id],
    );
}

/// 校验工作目录：支持 `~` 前缀；留空用主目录；目录必须存在
fn resolve_workdir(input: Option<&str>) -> Result<PathBuf, String> {
    let home = crate::discovery::home_dir();
    let dir = match input.map(str::trim).filter(|s| !s.is_empty()) {
        None => home,
        Some(s) => match s.strip_prefix('~') {
            Some(rest) => home.join(rest.trim_start_matches('/')),
            None => PathBuf::from(s),
        },
    };
    if dir.is_dir() {
        Ok(dir)
    } else {
        Err(format!("工作目录不存在：{}", dir.display()))
    }
}

/// 执行一轮对话：无论成败，结束后把它从「运行中」摘掉。
async fn run_turn(app: AppHandle, state: Arc<AppState>, turn: Turn) {
    let session_id = turn.session_id;
    run_turn_inner(app, state.clone(), turn).await;
    state.live.lock().unwrap().remove(&session_id);
}

/// 拉起 Agent 进程，逐行解析输出，流式推送到前端，结束后落库。
async fn run_turn_inner(app: AppHandle, state: Arc<AppState>, turn: Turn) {
    let session_id = turn.session_id;
    let Some(bin) = adapters::resolve_binary(&turn.adapter) else {
        fail(
            &app,
            &state,
            session_id,
            &format!(
                "未找到 {} 的可执行文件，请确认已安装后到 Agents 页重新扫描",
                turn.adapter
            ),
        );
        return;
    };
    let spec = TurnSpec {
        adapter: &turn.adapter,
        bin: &bin,
        prompt: &turn.prompt,
        model: turn.model.as_deref(),
        resume: turn.resume.as_deref(),
        workdir: &turn.workdir,
    };
    let mut child = match adapters::build_command(&spec).spawn() {
        Ok(child) => child,
        Err(e) => {
            fail(
                &app,
                &state,
                session_id,
                &format!("启动 {} 失败：{e}", bin.display()),
            );
            return;
        }
    };
    let stderr = child.stderr.take();
    let stdout = child.stdout.take();

    // 登记为运行中，供 chat_stop 终止；停止请求可能先于登记到达
    if state.cancelled.lock().unwrap().contains(&session_id) {
        let _ = child.start_kill();
    }
    state.running.lock().unwrap().insert(session_id, child);

    // 持续排空 stderr（写满缓冲区会阻塞子进程），并保留尾部用于异常诊断
    let stderr_tail = Arc::new(Mutex::new(String::new()));
    if let Some(stderr) = stderr {
        let tail = Arc::clone(&stderr_tail);
        tauri::async_runtime::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let mut tail = tail.lock().unwrap();
                        tail.push_str(&String::from_utf8_lossy(&buf));
                        let excess = tail.chars().count().saturating_sub(STDERR_TAIL_CHARS);
                        if excess > 0 {
                            let cut = tail
                                .char_indices()
                                .nth(excess)
                                .map_or(tail.len(), |(i, _)| i);
                            tail.drain(..cut);
                        }
                    }
                }
            }
        });
    }

    let mut st = TurnState::default();
    let mut failure: Option<String> = None;
    if let Some(stdout) = stdout {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let mut events = Vec::new();
            adapters::parse_line(&turn.adapter, &v, &mut st, &mut events);
            for event in events {
                match event {
                    TurnEvent::Session(id) => remember_ref(&state, session_id, &id),
                    TurnEvent::Delta(text) => {
                        emit(&app, &state, session_id, ChatEvent::Delta { session_id, text })
                    }
                    TurnEvent::Notice(text) => {
                        emit(&app, &state, session_id, ChatEvent::Notice { session_id, text })
                    }
                    TurnEvent::Failed(message) => failure = Some(message),
                }
            }
            // 终态已到：不必等进程退出（它拉起的子进程可能还占着输出管道）
            if st.finished {
                break;
            }
        }
    }

    // 收回子进程：被 chat_stop 取走时已经终止
    let child = state.running.lock().unwrap().remove(&session_id);
    let cancelled = state.cancelled.lock().unwrap().remove(&session_id);

    if let Some(message) = failure {
        fail(&app, &state, session_id, &message);
        reap(child);
        return;
    }
    if st.final_text.is_empty() {
        let mut message = if cancelled {
            "已停止".to_string()
        } else {
            "Agent 没有返回任何内容".to_string()
        };
        let stderr = stderr_tail.lock().unwrap().clone();
        if !cancelled && !stderr.is_empty() {
            message.push_str(&format!("：{}", adapters::clip(&stderr, 300)));
        }
        fail(&app, &state, session_id, &message);
        reap(child);
        return;
    }
    if !st.finished && !cancelled {
        // 没等到终态输出就断了：看进程是怎么退的，异常退出时提醒输出可能不完整
        let mut child = child;
        let status = match child.as_mut() {
            Some(child) => child.wait().await.ok(),
            None => None,
        };
        if status.is_some_and(|status| !status.success()) {
            let code = status
                .and_then(|status| status.code())
                .map_or_else(|| "信号".to_string(), |c| c.to_string());
            insert_message(
                &state,
                session_id,
                "system",
                &format!("Agent 进程以状态 {code} 退出，以上输出可能不完整"),
                None,
                None,
                None,
            );
        }
        complete(&app, &state, session_id, &st);
        return;
    }
    // 终态已到（或已停止）：内容是完整的，直接收尾。Agent 自己的收尾（保存会话、
    // 关掉拉起的 MCP 服务）可能要好几秒，放到后台等，不拖住界面。
    complete(&app, &state, session_id, &st);
    reap(child);
}

/// 本轮正常结束：回答落库，用完整文本校正流式预览后通知前端。
fn complete(app: &AppHandle, state: &AppState, session_id: i64, st: &TurnState) {
    insert_message(
        state,
        session_id,
        "assistant",
        &st.final_text,
        st.cost_usd,
        st.input_tokens,
        st.output_tokens,
    );
    emit(
        app,
        state,
        session_id,
        ChatEvent::Replace {
            session_id,
            text: st.final_text.clone(),
        },
    );
    emit(
        app,
        state,
        session_id,
        ChatEvent::Done {
            session_id,
            cost_usd: st.cost_usd,
            input_tokens: st.input_tokens,
            output_tokens: st.output_tokens,
        },
    );
}

/// 后台回收 Agent 进程：给它一点时间自行退出（保存会话、关闭拉起的子进程），
/// 超时就连同整个进程组一起结束。
fn reap(child: Option<tokio::process::Child>) {
    let Some(mut child) = child else {
        return;
    };
    tauri::async_runtime::spawn(async move {
        if tokio::time::timeout(EXIT_GRACE, child.wait()).await.is_err() {
            adapters::kill_process_group(&mut child);
            let _ = child.wait().await;
        }
    });
}

#[tauri::command]
pub fn chat_sessions(state: State<'_, Arc<AppState>>) -> Result<Vec<ChatSession>, String> {
    let db = state.db.lock().unwrap();
    let mut stmt = db
        .prepare(
            "SELECT id, adapter, title, model, workdir, created_at FROM sessions ORDER BY id DESC LIMIT 100",
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
                created_at: row.get(5)?,
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
            "SELECT id, session_id, role, content, cost_usd, input_tokens, output_tokens, created_at
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
                created_at: row.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

/// 开启新会话并发送第一条消息。model 为空时沿用 Agent 自身配置的默认模型。
#[tauri::command]
pub async fn chat_start(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    adapter: String,
    model: Option<String>,
    workdir: Option<String>,
    prompt: String,
) -> Result<i64, String> {
    if !adapters::is_supported(&adapter) {
        return Err(format!("「{adapter}」的会话适配尚未实现"));
    }
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err("消息不能为空".into());
    }
    let model = model.map(|m| m.trim().to_string()).filter(|m| !m.is_empty());
    let workdir = resolve_workdir(workdir.as_deref())?;
    let title: String = prompt.chars().take(24).collect();
    let sid = {
        let db = state.db.lock().unwrap();
        db.execute(
            "INSERT INTO sessions (adapter, title, model, workdir) VALUES (?1, ?2, ?3, ?4)",
            params![adapter, title, model, workdir.to_string_lossy()],
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
    // 先登记为运行中再启动：此后任何一次状态查询都能看到这个会话
    state.live.lock().unwrap().insert(sid, LiveTurn::default());
    let st = state.inner().clone();
    tauri::async_runtime::spawn(async move {
        run_turn(
            app,
            st,
            Turn {
                session_id: sid,
                adapter,
                resume: None,
                model,
                workdir,
                prompt,
            },
        )
        .await;
    });
    Ok(sid)
}

/// 在已有会话中续聊：沿用该会话的 Agent、模型与工作目录。
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
    let (adapter, resume, model, workdir) = {
        let db = state.db.lock().unwrap();
        db.query_row(
            "SELECT adapter, external_ref, model, workdir FROM sessions WHERE id = ?1",
            params![session_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .map_err(|_| format!("会话不存在：#{session_id}"))?
    };
    if !adapters::is_supported(&adapter) {
        return Err("该会话的 Agent 暂不支持续聊".into());
    }
    // 升级前创建的会话没有记录工作目录，退回主目录
    let workdir = resolve_workdir(workdir.as_deref())?;
    // 同一会话同一时间只跑一轮
    if state.live.lock().unwrap().contains_key(&session_id) {
        return Err("该会话上一轮还在运行，请等它结束或先停止".into());
    }
    {
        let db = state.db.lock().unwrap();
        db.execute(
            "INSERT INTO messages (session_id, role, content) VALUES (?1, 'user', ?2)",
            params![session_id, prompt],
        )
        .map_err(|e| e.to_string())?;
    }
    // 先登记为运行中再启动：此后任何一次状态查询都能看到这个会话
    state.live.lock().unwrap().insert(session_id, LiveTurn::default());
    let st = state.inner().clone();
    tauri::async_runtime::spawn(async move {
        run_turn(
            app,
            st,
            Turn {
                session_id,
                adapter,
                resume,
                model,
                workdir,
                prompt,
            },
        )
        .await;
    });
    Ok(())
}

/// 运行中的一轮对话目前的界面内容。
#[derive(Default)]
pub struct LiveTurn {
    text: String,
    activity: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveTurnInfo {
    pub session_id: i64,
    pub text: String,
    pub activity: Option<String>,
}

/// 正在运行的会话及其实时输出。界面以它为准：窗口不可见时推送事件可能没有送达，
/// 重新可见（或定时）时据此补登记、补收尾。
#[tauri::command]
pub fn chat_live(state: State<'_, Arc<AppState>>) -> Vec<LiveTurnInfo> {
    let live = state.live.lock().unwrap();
    let mut list: Vec<LiveTurnInfo> = live
        .iter()
        .map(|(id, turn)| LiveTurnInfo {
            session_id: *id,
            text: turn.text.clone(),
            activity: turn.activity.clone(),
        })
        .collect();
    list.sort_by_key(|turn| turn.session_id);
    list
}

/// 停止正在运行的一轮对话；已产出的文本会保留。
#[tauri::command]
pub fn chat_stop(state: State<'_, Arc<AppState>>, session_id: i64) -> Result<(), String> {
    state.cancelled.lock().unwrap().insert(session_id);
    if let Some(mut child) = state.running.lock().unwrap().remove(&session_id) {
        // 连同 Agent 拉起的子进程一起结束，再回收进程
        adapters::kill_process_group(&mut child);
        tauri::async_runtime::spawn(async move {
            let _ = child.wait().await;
        });
    }
    Ok(())
}
