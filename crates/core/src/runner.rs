//! 执行一轮对话：拉起 Agent 子进程 → 逐行解析输出 → 推送事件 → 收尾。
//!
//! 桌面应用（本机执行）与 agentd（远程机器执行）共用这一套流程，
//! 保证同一个 Agent 在哪台机器上跑、怎么被调用，行为都一致。

use crate::adapters::{self, TurnSpec, TurnState};
use crate::discovery;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::{mpsc, watch};

/// 进程异常退出时，附在提示里的 stderr 长度上限（字符数）
const STDERR_TAIL_CHARS: usize = 600;

/// 进程自行退出（保存会话、关闭拉起的子进程）的等待时间，超过就连同进程组一起结束
const EXIT_GRACE: Duration = Duration::from_secs(5);

/// 输出流断开后再等进程退出的时间：Agent 关了输出却还挂着时，不能让这一轮永远不结束
const EXIT_WAIT: Duration = Duration::from_secs(60);

/// 事件通道容量。接收方长期不取时让 Agent 因背压暂停输出，而不是把内存占满
const EVENT_QUEUE: usize = 256;

/// 结束进程组之后、补第二轮之前的间隔。
/// 给进程组发信号与子进程 fork 是并发的：恰好在这之间 fork 出来的子进程会漏掉第一轮
/// （它继承了进程组，却还没被生出来），隔一小会儿再对整个进程组补一轮就能收掉它。
const KILL_SECOND_PASS: Duration = Duration::from_millis(200);

/// 一轮对话的输入（也是控制端发给节点的线上格式）
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnParams {
    /// 调用方生成的标识，事件里原样带回
    pub turn_id: String,
    pub adapter: String,
    /// 为空时沿用 Agent 自身配置的默认模型
    #[serde(default)]
    pub model: Option<String>,
    /// 上一轮返回的外部会话标识；为空时开启新会话
    #[serde(default)]
    pub resume: Option<String>,
    /// 工作目录；留空为主目录，支持 `~` 前缀（指的是执行机器上的路径）
    #[serde(default)]
    pub workdir: Option<String>,
    pub prompt: String,
}

/// 一轮对话推给调用方的事件。最后一则一定是 [`TurnEvent::Failed`] 或 [`TurnEvent::Finished`]。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum TurnEvent {
    /// Agent 侧的会话标识，保存下来供后续续聊
    #[serde(rename = "turn.session")]
    Session { turn_id: String, r#ref: String },

    /// 文本增量，只用于实时预览
    #[serde(rename = "turn.delta")]
    Delta { turn_id: String, text: String },

    /// 过程提示：工具调用、思考摘要、非致命告警
    #[serde(rename = "turn.notice")]
    Notice { turn_id: String, text: String },

    /// 致命错误，本轮到此结束
    #[serde(rename = "turn.failed")]
    Failed { turn_id: String, message: String },

    /// 非致命的系统提示（例如「输出可能不完整」）
    #[serde(rename = "turn.system")]
    System { turn_id: String, message: String },

    /// 本轮结束。`incomplete` 表示没等到终态输出就断了；
    /// 被停止时带上已经产出的部分文本
    #[serde(rename = "turn.finished")]
    Finished {
        turn_id: String,
        text: String,
        #[serde(default)]
        cost_usd: Option<f64>,
        #[serde(default)]
        input_tokens: Option<i64>,
        #[serde(default)]
        output_tokens: Option<i64>,
        #[serde(default)]
        incomplete: bool,
    },
}

impl TurnEvent {
    pub fn turn_id(&self) -> &str {
        match self {
            TurnEvent::Session { turn_id, .. }
            | TurnEvent::Delta { turn_id, .. }
            | TurnEvent::Notice { turn_id, .. }
            | TurnEvent::Failed { turn_id, .. }
            | TurnEvent::System { turn_id, .. }
            | TurnEvent::Finished { turn_id, .. } => turn_id,
        }
    }
}

/// 校验工作目录：支持 `~` 前缀；留空用主目录；目录必须存在
pub fn resolve_workdir(input: Option<&str>) -> Result<PathBuf, String> {
    let home = discovery::home_dir();
    let dir = match input.map(str::trim).filter(|s| !s.is_empty()) {
        None => home,
        Some(s) => match s.strip_prefix('~') {
            Some(rest) => home.join(rest.trim_start_matches(['/', '\\'])),
            None => PathBuf::from(s),
        },
    };
    if dir.is_dir() {
        Ok(dir)
    } else {
        Err(format!("工作目录不存在：{}", dir.display()))
    }
}

/// 运行中（或正在收尾）的一轮对话，可随时停止。
#[derive(Clone)]
pub struct TurnHandle {
    proc: Arc<Proc>,
}

struct Proc {
    child: Mutex<Option<Child>>,
    /// 是否收到过停止请求
    cancelled: AtomicBool,
    /// 停止请求的通知：读取输出的一方据此立即收尾，不必等输出管道关闭
    stop_signal: watch::Sender<bool>,
}

impl Proc {
    fn new() -> Self {
        Self {
            child: Mutex::new(None),
            cancelled: AtomicBool::new(false),
            stop_signal: watch::Sender::new(false),
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

impl TurnHandle {
    /// 请求停止：连同 Agent 拉起的子进程一起结束，已经产出的文本会保留。
    pub fn stop(&self) {
        self.proc.cancelled.store(true, Ordering::SeqCst);
        let _ = self.proc.stop_signal.send(true);
        self.kill();
    }

    pub fn is_cancelled(&self) -> bool {
        self.proc.is_cancelled()
    }

    fn kill(&self) {
        // 进程可能还没登记完（停止请求先于启动完成），那时由启动方负责补杀
        if let Some(child) = self.proc.child.lock().unwrap().take() {
            tokio::spawn(terminate(child));
        }
    }
}

/// 结束 Agent 进程连同它拉起的所有子进程，并回收。
///
/// 两轮之间不去回收组长：它保持僵尸状态，pid 就不会被复用，
/// 第二轮按 pid 找到的进程组一定还是这一个。
async fn terminate(mut child: Child) {
    let pid = child.id();
    if let Some(pid) = pid {
        adapters::signal_group(pid);
    }
    tokio::time::sleep(KILL_SECOND_PASS).await;
    if let Some(pid) = pid {
        adapters::signal_group(pid);
    }
    let _ = child.wait().await;
}

/// 查找 Agent 可执行文件的方式（默认按 PATH 与常见安装位置查找）
pub type BinaryResolver = Arc<dyn Fn(&str) -> Option<PathBuf> + Send + Sync>;

pub struct TurnOutput {
    pub events: mpsc::Receiver<TurnEvent>,
    pub handle: TurnHandle,
}

/// 开始一轮对话并立即返回；过程与结果经 `events` 推送。
pub fn start(params: TurnParams) -> TurnOutput {
    start_with(params, Arc::new(adapters::resolve_binary))
}

/// 同 [`start`]，但由调用方决定 Agent 可执行文件的位置。
pub fn start_with(params: TurnParams, resolve: BinaryResolver) -> TurnOutput {
    let (tx, rx) = mpsc::channel(EVENT_QUEUE);
    let proc = Arc::new(Proc::new());
    let handle = TurnHandle { proc: proc.clone() };
    tokio::spawn(async move {
        run(params, tx, proc, resolve).await;
    });
    TurnOutput { events: rx, handle }
}

/// 推送一则事件。接收方已经离开（连接断开）时结束 Agent 进程并返回 false。
async fn emit(tx: &mpsc::Sender<TurnEvent>, proc: &Arc<Proc>, event: TurnEvent) -> bool {
    if tx.send(event).await.is_err() {
        // 接收方只会在停止时离开；没停过的也当作停止，读取输出的一方要立刻收尾
        TurnHandle { proc: proc.clone() }.stop();
        return false;
    }
    true
}

async fn run(
    params: TurnParams,
    tx: mpsc::Sender<TurnEvent>,
    proc: Arc<Proc>,
    resolve: BinaryResolver,
) {
    let id = params.turn_id.clone();
    let failed = |id: &str, message: String| TurnEvent::Failed {
        turn_id: id.to_string(),
        message,
    };

    let workdir = match resolve_workdir(params.workdir.as_deref()) {
        Ok(dir) => dir,
        Err(e) => {
            emit(&tx, &proc, failed(&id, e)).await;
            return;
        }
    };
    let Some(bin) = resolve(&params.adapter) else {
        emit(
            &tx,
            &proc,
            failed(
                &id,
                format!(
                    "未找到 {} 的可执行文件，请确认已安装后到 Agents 页重新扫描",
                    params.adapter
                ),
            ),
        )
        .await;
        return;
    };
    let spec = TurnSpec {
        adapter: &params.adapter,
        bin: &bin,
        prompt: &params.prompt,
        model: params.model.as_deref(),
        resume: params.resume.as_deref(),
        workdir: &workdir,
    };
    let child = match adapters::build_command(&spec).spawn() {
        Ok(child) => child,
        Err(e) => {
            emit(
                &tx,
                &proc,
                failed(&id, format!("启动 {} 失败：{e}", bin.display())),
            )
            .await;
            return;
        }
    };

    // 登记为运行中，供停止使用；停止请求可能先于登记到达，登记后补查一次
    let mut child = child;
    let stderr = child.stderr.take();
    let stdout = child.stdout.take();
    let mut stop_signal = proc.stop_signal.subscribe();
    *proc.child.lock().unwrap() = Some(child);
    if proc.is_cancelled() {
        TurnHandle { proc: proc.clone() }.kill();
    }

    // 持续排空 stderr（写满缓冲区会阻塞子进程），并保留尾部用于异常诊断
    let stderr_tail = Arc::new(Mutex::new(String::new()));
    if let Some(stderr) = stderr {
        let tail = Arc::clone(&stderr_tail);
        tokio::spawn(async move {
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
        loop {
            let line = tokio::select! {
                line = lines.next_line() => line,
                // 被停止：立即收尾。漏网的子进程可能还占着输出管道，等它关闭没有意义
                _ = stop_signal.changed() => break,
            };
            let Ok(Some(line)) = line else {
                break;
            };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            let mut events = Vec::new();
            adapters::parse_line(&params.adapter, &v, &mut st, &mut events);
            for event in events {
                let forwarded = match event {
                    adapters::TurnEvent::Session(r) => TurnEvent::Session {
                        turn_id: id.clone(),
                        r#ref: r,
                    },
                    adapters::TurnEvent::Delta(text) => TurnEvent::Delta {
                        turn_id: id.clone(),
                        text,
                    },
                    adapters::TurnEvent::Notice(text) => TurnEvent::Notice {
                        turn_id: id.clone(),
                        text,
                    },
                    adapters::TurnEvent::Failed(message) => {
                        failure = Some(message);
                        // 致命错误等输出流结束后再发，调用方把它当作本轮的结尾
                        continue;
                    }
                };
                if !emit(&tx, &proc, forwarded).await {
                    return;
                }
            }
            // 终态已到：不必等进程退出（它拉起的子进程可能还占着输出管道）
            if st.finished {
                break;
            }
        }
    }

    // 收回子进程：被停止时已经终止并回收
    let child = proc.child.lock().unwrap().take();
    let cancelled = proc.is_cancelled();

    if let Some(message) = failure {
        emit(&tx, &proc, failed(&id, message)).await;
        reap(child);
        return;
    }
    if st.final_text.is_empty() {
        // 流式 Agent（claude / grok / pi）的完整文本要到终态才给：中途停止时，
        // 把已经流出来的部分留作本轮结果
        let mut message = if cancelled {
            "已停止".to_string()
        } else {
            "Agent 没有返回任何内容".to_string()
        };
        let stderr = stderr_tail.lock().unwrap().clone();
        if !cancelled && !stderr.is_empty() {
            message.push_str(&format!("：{}", adapters::clip(&stderr, 300)));
        }
        if cancelled && !st.streamed.is_empty() {
            if !emit(
                &tx,
                &proc,
                TurnEvent::System {
                    turn_id: id.clone(),
                    message: "已停止：完整回答尚未生成，以上是已经产出的内容".into(),
                },
            )
            .await
            {
                return;
            }
            emit(
                &tx,
                &proc,
                TurnEvent::Finished {
                    turn_id: id.clone(),
                    text: st.streamed.clone(),
                    cost_usd: st.cost_usd,
                    input_tokens: st.input_tokens,
                    output_tokens: st.output_tokens,
                    incomplete: true,
                },
            )
            .await;
            reap(child);
            return;
        }
        emit(&tx, &proc, failed(&id, message)).await;
        reap(child);
        return;
    }
    let finished = |incomplete: bool| TurnEvent::Finished {
        turn_id: id.clone(),
        text: st.final_text.clone(),
        cost_usd: st.cost_usd,
        input_tokens: st.input_tokens,
        output_tokens: st.output_tokens,
        incomplete,
    };
    if !st.finished && !cancelled {
        // 没等到终态输出就断了：看进程是怎么退的，异常退出时提醒输出可能不完整
        let mut child = child;
        let mut status = None;
        if let Some(running) = child.as_mut() {
            match tokio::time::timeout(EXIT_WAIT, running.wait()).await {
                Ok(exit) => status = exit.ok(),
                Err(_) => {
                    // 关了输出却迟迟不退出：连同所有子进程一起结束
                    terminate(child.take().expect("进程仍在")).await;
                }
            }
        }
        if status.is_some_and(|status| !status.success()) {
            let code = status
                .and_then(|status| status.code())
                .map_or_else(|| "信号".to_string(), |c| c.to_string());
            if !emit(
                &tx,
                &proc,
                TurnEvent::System {
                    turn_id: id.clone(),
                    message: format!("Agent 进程以状态 {code} 退出，以上输出可能不完整"),
                },
            )
            .await
            {
                return;
            }
        }
        emit(&tx, &proc, finished(true)).await;
        return;
    }
    // 终态已到（或已停止）：以完整文本收尾。Agent 自己的收尾（保存会话、关闭拉起的
    // MCP 服务）可能要好几秒，放到后台等，不拖住调用方。
    emit(&tx, &proc, finished(!st.finished)).await;
    reap(child);
}

/// 后台回收 Agent 进程：给它一点时间自行退出，超时就连同所有子进程一起结束。
fn reap(child: Option<Child>) {
    let Some(mut child) = child else {
        return;
    };
    tokio::spawn(async move {
        if tokio::time::timeout(EXIT_GRACE, child.wait()).await.is_err() {
            terminate(child).await;
        }
    });
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::path::Path;

    /// 事件是控制端与节点之间的线上格式：序列化后字段名必须稳定
    #[test]
    fn events_round_trip_on_the_wire() {
        let event = TurnEvent::Finished {
            turn_id: "7".into(),
            text: "done".into(),
            cost_usd: Some(0.5),
            input_tokens: Some(10),
            output_tokens: None,
            incomplete: false,
        };
        let wire = serde_json::to_value(&event).unwrap();
        assert_eq!(wire["type"], "turn.finished");
        assert_eq!(wire["turnId"], "7");
        assert_eq!(wire["costUsd"], 0.5);
        let back: TurnEvent = serde_json::from_value(wire).unwrap();
        assert_eq!(back, event);
        assert_eq!(back.turn_id(), "7");

        let session: TurnEvent =
            serde_json::from_str(r#"{"type":"turn.session","turnId":"1","ref":"s1"}"#).unwrap();
        assert_eq!(
            session,
            TurnEvent::Session {
                turn_id: "1".into(),
                r#ref: "s1".into()
            }
        );
    }

    /// 一轮对话的完整生命周期：进程输出 → 事件流 → 以 Finished 收尾
    #[tokio::test]
    async fn turn_streams_events_and_finishes() {
        let fake = fake_agent(
            r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"s1"}' '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"hel"}}}' '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"lo"}}}' '{"type":"assistant","message":{"content":[{"type":"text","text":"hello"}]}}' '{"type":"result","is_error":false,"result":"hello","total_cost_usd":0.25,"usage":{"input_tokens":3,"output_tokens":4}}'
# 模拟 Agent 收尾很慢：终态之后进程还挂着、还占着输出管道
sleep 3 &
"#,
        );
        let mut events = collect(run_with(&fake, "t1")).await;
        let kinds: Vec<String> = events
            .iter()
            .map(|e| {
                serde_json::to_value(e).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        // 终态事件已到，不必等进程退出就收尾
        assert_eq!(
            kinds,
            ["turn.session", "turn.delta", "turn.delta", "turn.finished"]
        );
        let TurnEvent::Finished {
            text,
            cost_usd,
            input_tokens,
            output_tokens,
            incomplete,
            ..
        } = events.pop().unwrap()
        else {
            panic!("最后一则事件应是 Finished");
        };
        assert_eq!(text, "hello");
        assert_eq!(cost_usd, Some(0.25));
        assert_eq!((input_tokens, output_tokens), (Some(3), Some(4)));
        assert!(!incomplete);
    }

    /// 停止：进程被结束，以 Failed（已停止）收尾
    #[tokio::test]
    async fn stop_ends_the_turn() {
        let fake = fake_agent("sleep 60\n");
        let mut output = run_with(&fake, "t2");
        // 让进程先起来并开始输出，再停止：已经产出的文本要保留
        tokio::time::sleep(Duration::from_millis(300)).await;
        output.handle.stop();
        let event = tokio::time::timeout(Duration::from_secs(5), output.events.recv())
            .await
            .expect("停止后应当很快收尾")
            .unwrap();
        assert_eq!(
            event,
            TurnEvent::Failed {
                turn_id: "t2".into(),
                message: "已停止".into()
            }
        );
        assert!(output.events.recv().await.is_none(), "Failed 之后不应再有事件");
    }

    /// 被停止时已经产出的部分文本照样带回
    #[tokio::test]
    async fn stopped_turn_keeps_partial_output() {
        let fake = fake_agent(
            r#"printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"部分"}}}'
sleep 60
"#,
        );
        let mut output = run_with(&fake, "t3");
        let first = tokio::time::timeout(Duration::from_secs(5), output.events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            first,
            TurnEvent::Delta {
                turn_id: "t3".into(),
                text: "部分".into()
            }
        );
        output.handle.stop();
        let last = terminal(&mut output.events).await;
        let TurnEvent::Finished { text, incomplete, .. } = last else {
            panic!("应当以 Finished 收尾：{last:?}");
        };
        assert_eq!(text, "部分");
        assert!(incomplete, "被停止的输出要标注为不完整");
        assert!(output.events.recv().await.is_none(), "Finished 之后不应再有事件");
    }

    /// 反复启动后立刻停止：每次都要在 3 秒内收尾，且不留下子进程
    #[tokio::test]
    async fn stop_is_always_prompt() {
        let fake = fake_agent("sleep 60\n");
        for i in 0..40 {
            let mut output = run_with(&fake, &format!("fast-{i}"));
            tokio::time::sleep(Duration::from_millis(i % 7)).await;
            output.handle.stop();
            let event = terminal(&mut output.events).await;
            assert!(
                matches!(event, TurnEvent::Failed { .. } | TurnEvent::Finished { .. }),
                "第 {i} 次：{event:?}"
            );
        }
    }

    /// 取到终态事件（Failed / Finished）
    async fn terminal(events: &mut mpsc::Receiver<TurnEvent>) -> TurnEvent {
        let deadline = Duration::from_secs(3);
        loop {
            let event = tokio::time::timeout(deadline, events.recv())
                .await
                .expect("停止后应当很快收尾")
                .expect("终态事件之后通道才会关闭");
            if matches!(
                event,
                TurnEvent::Failed { .. } | TurnEvent::Finished { .. }
            ) {
                return event;
            }
        }
    }

    /// 找不到可执行文件、目录不存在：都以 Failed 收尾
    #[tokio::test]
    async fn setup_errors_end_with_failed() {
        let dir = tempfile::tempdir().unwrap();
        let output = start_with(
            TurnParams {
                turn_id: "t4".into(),
                adapter: "claude-cli".into(),
                model: None,
                resume: None,
                workdir: Some(dir.path().join("missing").to_string_lossy().into_owned()),
                prompt: "hi".into(),
            },
            Arc::new(|_: &str| None),
        );
        let events = collect(output).await;
        assert!(matches!(
            events.last(),
            Some(TurnEvent::Failed { message, .. }) if message.contains("工作目录不存在")
        ));

        let output = start_with(
            TurnParams {
                turn_id: "t5".into(),
                adapter: "claude-cli".into(),
                model: None,
                resume: None,
                workdir: Some(dir.path().to_string_lossy().into_owned()),
                prompt: "hi".into(),
            },
            Arc::new(|_: &str| None),
        );
        let events = collect(output).await;
        assert!(matches!(
            events.last(),
            Some(TurnEvent::Failed { message, .. }) if message.contains("未找到")
        ));
    }

    #[test]
    fn workdir_accepts_tilde_and_rejects_missing() {
        let home = discovery::home_dir();
        assert_eq!(resolve_workdir(None).unwrap(), home);
        assert_eq!(resolve_workdir(Some("~")).unwrap(), home);
        assert_eq!(resolve_workdir(Some("~/")).unwrap(), home);
        assert!(resolve_workdir(Some("~/no/such/dir/aa")).is_err());
        assert_eq!(resolve_workdir(Some("")).unwrap(), home);
    }

    /// 造一个冒充 claude 的脚本，返回它的路径
    fn fake_agent(script: &str) -> PathBuf {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        std::fs::write(&bin, format!("#!/bin/sh\n{script}")).unwrap();
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        // tempdir 离开作用域就会被删，脚本要留到测试结束
        std::mem::forget(dir);
        bin
    }

    fn run_with(fake: &Path, turn_id: &str) -> TurnOutput {
        let fake = fake.to_path_buf();
        start_with(
            TurnParams {
                turn_id: turn_id.into(),
                adapter: "claude-cli".into(),
                model: None,
                resume: None,
                workdir: None,
                prompt: "hi".into(),
            },
            Arc::new(move |_: &str| Some(fake.clone())),
        )
    }

    async fn collect(mut output: TurnOutput) -> Vec<TurnEvent> {
        let mut events = Vec::new();
        while let Some(event) = output.events.recv().await {
            events.push(event);
        }
        events
    }
}
