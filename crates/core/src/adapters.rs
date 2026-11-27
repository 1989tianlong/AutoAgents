//! CLI Agent 会话适配层：把「一轮对话」归一到统一的命令构造与输出解析。
//!
//! 四种 Agent 的无头模式各不相同：
//! - claude-cli：`claude -p --output-format stream-json --include-partial-messages`
//! - codex-cli ：`codex exec --json`，续聊为 `codex exec resume <thread_id>`
//! - grok-cli  ：`grok -p --output-format streaming-messages-json`（输出与 claude 同构）
//! - pi        ：`pi -p --mode json`
//!
//! 这里只做两件事：[`build_command`] 拼出子进程命令；[`parse_line`] 把 stdout 的
//! 逐行 JSON 解析成统一的 [`TurnEvent`]，并累积到 [`TurnState`]。
//! 协议均按本机实测版本验证（claude 2.1 / codex 0.169 / grok 1.0.92 / pi 1.5.2）。

use crate::discovery;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::{Child as CommandChild, Command};

/// 已实现会话的 adapter → 二进制名
const ADAPTERS: &[(&str, &str)] = &[
    ("claude-cli", "claude"),
    ("codex-cli", "codex"),
    ("grok-cli", "grok"),
    ("pi", "pi"),
];

pub fn is_supported(adapter: &str) -> bool {
    ADAPTERS.iter().any(|(a, _)| *a == adapter)
}

/// 定位 adapter 对应的可执行文件（PATH + 常见安装位置）。
pub fn resolve_binary(adapter: &str) -> Option<PathBuf> {
    let (_, bin) = ADAPTERS.iter().find(|(a, _)| *a == adapter)?;
    discovery::find_binary(bin, &discovery::search_dirs())
}

/// 一轮对话的输入。
pub struct TurnSpec<'a> {
    pub adapter: &'a str,
    pub bin: &'a Path,
    pub prompt: &'a str,
    /// 为 None 时沿用 Agent 自身配置的默认模型
    pub model: Option<&'a str>,
    /// 上一轮返回的外部会话标识；为 None 时开启新会话
    pub resume: Option<&'a str>,
    /// Agent 的工作目录。grok / pi 的会话按目录归档，同一会话必须保持一致
    pub workdir: &'a Path,
}

/// 解析输出的过程中产生的事件。
#[derive(Debug)]
pub enum TurnEvent {
    /// 外部会话标识，保存下来供后续续聊
    Session(String),
    /// 文本增量，只用于实时预览
    Delta(String),
    /// 过程提示：工具调用、思考摘要、非致命告警
    Notice(String),
    /// 致命错误
    Failed(String),
}

/// 一轮对话中累积的状态；`final_text` 是落库的权威内容。
#[derive(Default)]
pub struct TurnState {
    pub final_text: String,
    pub cost_usd: Option<f64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    /// 本轮是否已收到终态；此后不必再等进程退出
    pub finished: bool,
    /// 本轮是否出现过流式增量
    streamed_any: bool,
    /// 流式增量累计的预览文本：中途停止时，完整文本还没到，用它作为本轮结果
    pub streamed: String,
    /// claude / grok：是否开启了逐字流式（工具调用提示已由 stream_event 给出）
    partial_seen: bool,
    thinking_notified: bool,
}

impl TurnState {
    /// 记录一段权威文本。Agent 不提供流式增量时（codex），同时作为增量实时推送；
    /// 提供增量时（claude / grok / pi），增量已经推送过，这里只累积不再推送。
    fn push_text(&mut self, text: &str) -> Option<TurnEvent> {
        self.final_text.push_str(text);
        if self.streamed_any {
            None
        } else {
            Some(TurnEvent::Delta(text.to_string()))
        }
    }

    /// 记录流式增量，只用于实时预览
    fn push_delta(&mut self, text: &str) -> TurnEvent {
        self.streamed_any = true;
        self.streamed.push_str(text);
        TurnEvent::Delta(text.to_string())
    }

    /// 一轮对话的总费用（claude / grok 在终态里直接给出）
    fn set_cost(&mut self, cost: Option<f64>) {
        if let Some(c) = cost {
            self.cost_usd = Some(c);
        }
    }

    /// 累加一条消息的费用（pi 按消息给出）
    fn add_cost(&mut self, cost: Option<f64>) {
        if let Some(c) = cost {
            self.cost_usd = Some(self.cost_usd.unwrap_or(0.0) + c);
        }
    }

    fn add_tokens(&mut self, input: Option<i64>, output: Option<i64>) {
        if input.is_some() || output.is_some() {
            self.input_tokens = Some(self.input_tokens.unwrap_or(0) + input.unwrap_or(0));
            self.output_tokens = Some(self.output_tokens.unwrap_or(0) + output.unwrap_or(0));
        }
    }

    fn notify_thinking(&mut self) -> Option<TurnEvent> {
        if self.thinking_notified {
            return None;
        }
        self.thinking_notified = true;
        Some(TurnEvent::Notice("思考中…".into()))
    }
}

/// 追加 `<flag> <model>`；未指定模型时沿用 Agent 自身配置。
fn add_model(cmd: &mut Command, model: Option<&str>, flag: &str) {
    if let Some(m) = model {
        cmd.arg(flag).arg(m);
    }
}

/// 构造本轮对话的子进程命令。stdin 关闭，否则 codex 会等待 stdin 追加输入。
pub fn build_command(spec: &TurnSpec) -> Command {
    let mut cmd = Command::new(spec.bin);
    match spec.adapter {
        "codex-cli" => match spec.resume {
            Some(id) => {
                cmd.args(["exec", "resume", "--json", "--skip-git-repo-check"]);
                add_model(&mut cmd, spec.model, "-m");
                cmd.arg(id).arg(spec.prompt);
            }
            None => {
                cmd.args(["exec", "--json", "--skip-git-repo-check"]);
                add_model(&mut cmd, spec.model, "-m");
                cmd.arg(spec.prompt);
            }
        },
        "grok-cli" => {
            cmd.args([
                "-p",
                spec.prompt,
                "--output-format",
                "streaming-messages-json",
                "--include-partial-messages",
                "--verbatim",
            ]);
            add_model(&mut cmd, spec.model, "-m");
            if let Some(id) = spec.resume {
                cmd.args(["-r", id]);
            }
            cmd.arg("--cwd").arg(spec.workdir);
        }
        "pi" => {
            cmd.args(["-p", spec.prompt, "--mode", "json"]);
            add_model(&mut cmd, spec.model, "--model");
            if let Some(id) = spec.resume {
                cmd.args(["--session", id]);
            }
        }
        // claude-cli
        _ => {
            cmd.args([
                "-p",
                spec.prompt,
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
            ]);
            add_model(&mut cmd, spec.model, "--model");
            if let Some(id) = spec.resume {
                cmd.args(["--resume", id]);
            }
        }
    }
    cmd.current_dir(spec.workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("PATH", discovery::child_path());
    // 独立进程组：停止时能把 Agent 拉起的子进程（MCP 服务、npx 等）一并结束
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.as_std_mut().process_group(0);
    }
    // Windows：从图形界面启动时不弹出控制台窗口
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.as_std_mut().creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(any(unix, windows)))]
    let _ = &mut cmd;
    cmd
}

/// Unix：向整个进程组发 SIGKILL（负 pid 表示进程组）。
///
/// 只发信号、不回收：组长保持僵尸状态，它的 pid 就不会被别的进程复用，
/// 之后再次按这个 pid 找进程组一定是安全的。
#[cfg(unix)]
pub fn signal_group(pid: u32) {
    // 禁止 pid <= 1：kill(0) 会杀自身进程组，kill(-1) 会杀全系统所有可发信号进程
    if pid <= 1 || pid > i32::MAX as u32 {
        return;
    }
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

/// Windows：按进程树结束（`taskkill /T`）。
#[cfg(windows)]
pub fn signal_group(pid: u32) {
    if pid <= 1 {
        return;
    }
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// 结束子进程连同它拉起的所有子进程。
/// Unix 上子进程是独立进程组的组长，整组结束；Windows 上按进程树结束。
pub fn kill_process_group(child: &mut CommandChild) {
    if let Some(pid) = child.id() {
        signal_group(pid);
    }
    let _ = child.start_kill();
}

/// 截断为单行，用于过程提示与错误信息。
pub fn clip(text: &str, max: usize) -> String {
    let line = text.trim().lines().next().unwrap_or("");
    if line.chars().count() <= max {
        line.to_string()
    } else {
        let head: String = line.chars().take(max).collect();
        format!("{head}…")
    }
}

/// 解析一行 stdout 输出（JSON），事件追加到 `out`。
pub fn parse_line(adapter: &str, v: &Value, st: &mut TurnState, out: &mut Vec<TurnEvent>) {
    match adapter {
        "codex-cli" => parse_codex(v, st, out),
        "pi" => parse_pi(v, st, out),
        // claude-cli / grok-cli：同一套 stream-json 协议
        _ => parse_claude_like(v, st, out),
    }
}

/// claude-cli 与 grok-cli：逐字增量在 stream_event 里，完整文本在 assistant 消息里。
fn parse_claude_like(v: &Value, st: &mut TurnState, out: &mut Vec<TurnEvent>) {
    match v["type"].as_str().unwrap_or("") {
        "system" => match v["subtype"].as_str().unwrap_or("") {
            "init" => {
                if let Some(id) = v["session_id"].as_str() {
                    out.push(TurnEvent::Session(id.to_string()));
                }
            }
            // 限流 / 额度耗尽时 CLI 会退避重试很多轮（可能长达数分钟），
            // 及时告知用户，便于停止并换模型
            "api_retry" => out.push(TurnEvent::Notice(format!(
                "请求被拒（{} {}），{} 秒后重试 {}/{}…",
                v["error_status"].as_u64().unwrap_or(0),
                v["error"].as_str().unwrap_or(""),
                v["retry_delay_ms"].as_u64().unwrap_or(0) / 1000,
                v["attempt"].as_u64().unwrap_or(0),
                v["max_retries"].as_u64().unwrap_or(0),
            ))),
            _ => {}
        },
        "stream_event" => {
            st.partial_seen = true;
            let ev = &v["event"];
            match ev["type"].as_str().unwrap_or("") {
                "content_block_start" => {
                    let block = &ev["content_block"];
                    if block["type"].as_str() == Some("tool_use") {
                        out.push(TurnEvent::Notice(format!(
                            "调用工具 {}",
                            block["name"].as_str().unwrap_or("")
                        )));
                    }
                }
                "content_block_delta" => match ev["delta"]["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        if let Some(text) = ev["delta"]["text"].as_str() {
                            out.push(st.push_delta(text));
                        }
                    }
                    "thinking_delta" => out.extend(st.notify_thinking()),
                    _ => {}
                },
                _ => {}
            }
        }
        "assistant" => {
            // 子 Agent 的消息带 parent_tool_use_id，不属于主回答
            if !v["parent_tool_use_id"].is_null() {
                return;
            }
            let Some(blocks) = v["message"]["content"].as_array() else {
                return;
            };
            for block in blocks {
                match block["type"].as_str().unwrap_or("") {
                    "text" => {
                        if let Some(text) = block["text"].as_str() {
                            out.extend(st.push_text(text));
                        }
                    }
                    // 逐字流式时工具调用提示已由 content_block_start 给出
                    "tool_use" if !st.partial_seen => out.push(TurnEvent::Notice(format!(
                        "调用工具 {}",
                        block["name"].as_str().unwrap_or("")
                    ))),
                    _ => {}
                }
            }
        }
        "result" => {
            st.finished = true;
            if let Some(id) = v["session_id"].as_str() {
                out.push(TurnEvent::Session(id.to_string()));
            }
            st.set_cost(v["total_cost_usd"].as_f64());
            st.add_tokens(
                v["usage"]["input_tokens"].as_i64(),
                v["usage"]["output_tokens"].as_i64(),
            );
            let result = v["result"].as_str().unwrap_or("");
            if v["is_error"].as_bool() == Some(true) {
                out.push(TurnEvent::Failed(if result.is_empty() {
                    "Agent 执行出错".into()
                } else {
                    result.to_string()
                }));
            } else if st.final_text.is_empty() && !result.is_empty() {
                out.extend(st.push_text(result));
            }
        }
        _ => {}
    }
}

/// codex-cli：没有逐字增量，回答按整条 agent_message 给出；会话标识叫 thread_id。
fn parse_codex(v: &Value, st: &mut TurnState, out: &mut Vec<TurnEvent>) {
    let kind = v["type"].as_str().unwrap_or("");
    match kind {
        "thread.started" => {
            if let Some(id) = v["thread_id"].as_str() {
                out.push(TurnEvent::Session(id.to_string()));
            }
        }
        // 旧版本协议
        "session_configured" => {
            if let Some(id) = v["session_id"].as_str() {
                out.push(TurnEvent::Session(id.to_string()));
            }
        }
        "agent_message" => {
            if let Some(text) = v["message"].as_str() {
                out.extend(st.push_text(text));
            }
        }
        "task_complete" => {
            st.finished = true;
            if st.final_text.is_empty()
                && let Some(text) = v["last_agent_message"].as_str()
            {
                out.extend(st.push_text(text));
            }
        }
        "item.started" | "item.updated" | "item.completed" => {
            let item = &v["item"];
            let done = kind == "item.completed";
            match item["type"].as_str().unwrap_or("") {
                "agent_message" => {
                    // 当前版本按整条给出；若某版本提供 text_delta 则走增量
                    if let Some(delta) = item["text_delta"].as_str() {
                        out.push(st.push_delta(delta));
                    } else if done && let Some(text) = item["text"].as_str() {
                        out.extend(st.push_text(text));
                    }
                }
                "reasoning" if done => out.push(TurnEvent::Notice(format!(
                    "思考：{}",
                    clip(item["text"].as_str().unwrap_or(""), 80)
                ))),
                "command_execution" if !done => out.push(TurnEvent::Notice(format!(
                    "运行命令：{}",
                    clip(item["command"].as_str().unwrap_or(""), 80)
                ))),
                "file_change" if !done => out.push(TurnEvent::Notice("修改文件".into())),
                "mcp_tool_call" if !done => out.push(TurnEvent::Notice(format!(
                    "MCP：{}/{}",
                    item["server"].as_str().unwrap_or(""),
                    item["tool"].as_str().unwrap_or("")
                ))),
                "web_search" if !done => out.push(TurnEvent::Notice("联网搜索".into())),
                // 多为配置告警，不影响本轮执行
                "error" => out.push(TurnEvent::Notice(clip(
                    item["message"].as_str().unwrap_or(""),
                    160,
                ))),
                _ => {}
            }
        }
        "turn.completed" => {
            st.finished = true;
            st.add_tokens(
                v["usage"]["input_tokens"].as_i64(),
                v["usage"]["output_tokens"].as_i64(),
            );
        }
        "turn.failed" | "error" => {
            st.finished = true;
            let message = v["error"]["message"]
                .as_str()
                .or_else(|| v["message"].as_str())
                .unwrap_or("Codex 执行出错");
            out.push(TurnEvent::Failed(message.to_string()));
        }
        _ => {}
    }
}

/// pi：逐字增量在 message_update 里，完整文本与费用在 message_end 里；会话标识在首行。
fn parse_pi(v: &Value, st: &mut TurnState, out: &mut Vec<TurnEvent>) {
    match v["type"].as_str().unwrap_or("") {
        "session" => {
            if let Some(id) = v["id"].as_str() {
                out.push(TurnEvent::Session(id.to_string()));
            }
        }
        "message_update" => {
            let event = &v["assistantMessageEvent"];
            match event["type"].as_str().unwrap_or("") {
                "text_delta" => {
                    if let Some(text) = event["delta"].as_str() {
                        out.push(st.push_delta(text));
                    }
                }
                "thinking_delta" => out.extend(st.notify_thinking()),
                _ => {}
            }
        }
        "message_end" => {
            let message = &v["message"];
            if message["role"].as_str() != Some("assistant") {
                return;
            }
            let text: String = message["content"]
                .as_array()
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter(|b| b["type"].as_str() == Some("text"))
                        .filter_map(|b| b["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default();
            if !text.is_empty() {
                out.extend(st.push_text(&text));
            }
            let usage = &message["usage"];
            st.add_tokens(usage["input"].as_i64(), usage["output"].as_i64());
            st.add_cost(usage["cost"]["total"].as_f64());
        }
        "tool_execution_start" => out.push(TurnEvent::Notice(format!(
            "调用工具 {}",
            v["toolName"].as_str().unwrap_or("")
        ))),
        // 旧版本协议
        "text" => {
            if let Some(text) = v["text"].as_str() {
                out.extend(st.push_text(text));
            }
        }
        "done" => {
            st.finished = true;
            st.add_tokens(v["usage"]["input"].as_i64(), v["usage"]["output"].as_i64());
            st.add_cost(v["usage"]["cost"]["total"].as_f64());
        }
        "agent_end" => st.finished = true,
        "error" => {
            st.finished = true;
            let message = v["message"]
                .as_str()
                .or_else(|| v["error"].as_str())
                .unwrap_or("Pi 执行出错");
            out.push(TurnEvent::Failed(message.to_string()));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(adapter: &str, lines: &[&str]) -> (TurnState, Vec<TurnEvent>) {
        let mut st = TurnState::default();
        let mut out = Vec::new();
        for line in lines {
            parse_line(
                adapter,
                &serde_json::from_str(line).unwrap(),
                &mut st,
                &mut out,
            );
        }
        (st, out)
    }

    fn deltas(out: &[TurnEvent]) -> Vec<&str> {
        out.iter()
            .filter_map(|e| match e {
                TurnEvent::Delta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    /// 增量只用于预览：落库文本必须等于完整消息，且不能被重复推送。
    #[test]
    fn claude_stream_deltas_are_not_duplicated() {
        let (st, out) = run(
            "claude-cli",
            &[
                r#"{"type":"system","subtype":"init","session_id":"s1"}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"1\n"}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"t"}}}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"2"}}}"#,
                // 子 Agent 的消息不应计入主回答
                r#"{"type":"assistant","parent_tool_use_id":"toolu_1","message":{"content":[{"type":"text","text":"noise"}]}}"#,
                r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"text","text":"1\n2"}]}}"#,
                r#"{"type":"result","is_error":false,"result":"1\n2","total_cost_usd":0.5,"usage":{"input_tokens":10,"output_tokens":2}}"#,
            ],
        );
        assert_eq!(st.final_text, "1\n2");
        assert_eq!(st.cost_usd, Some(0.5));
        assert!(st.finished, "result 是终态");
        assert_eq!((st.input_tokens, st.output_tokens), (Some(10), Some(2)));
        assert_eq!(deltas(&out), ["1\n", "2"]);
        assert!(matches!(&out[0], TurnEvent::Session(id) if id == "s1"));
        // 思考提示只出现一次
        assert_eq!(
            out.iter()
                .filter(|e| matches!(e, TurnEvent::Notice(_)))
                .count(),
            1
        );
    }

    /// 限流重试要提示出来：默认模型额度耗尽时 CLI 会静默重试十来分钟
    #[test]
    fn claude_api_retry_is_surfaced() {
        let (st, out) = run(
            "claude-cli",
            &[
                r#"{"type":"system","subtype":"api_retry","attempt":1,"max_retries":10,"retry_delay_ms":60000,"error_status":429,"error":"rate_limit"}"#,
            ],
        );
        assert!(!st.finished);
        assert!(
            matches!(&out[0], TurnEvent::Notice(t) if t == "请求被拒（429 rate_limit），60 秒后重试 1/10…")
        );
    }

    /// grok 与 claude 同构，走同一解析器
    #[test]
    fn grok_shares_claude_protocol() {
        let (st, out) = run(
            "grok-cli",
            &[
                r#"{"type":"system","subtype":"init","session_id":"g1","model":"m"}"#,
                r#"{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"tool_use","name":"read_file"}}}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"read_file"}]}}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"done"}]}}"#,
                r#"{"type":"result","is_error":false,"result":"done","total_cost_usd":0.03}"#,
            ],
        );
        assert_eq!(st.final_text, "done");
        assert_eq!(st.cost_usd, Some(0.03));
        // 逐字流式下，工具调用提示只由 content_block_start 给出一次
        assert_eq!(deltas(&out), ["done"]);
        assert_eq!(
            out.iter()
                .filter(|e| matches!(e, TurnEvent::Notice(_)))
                .count(),
            1
        );
    }

    /// codex 没有增量：整条消息既落库也实时推送；item 级 error 只是非致命告警
    #[test]
    fn codex_whole_messages_stream_through() {
        let (st, out) = run(
            "codex-cli",
            &[
                r#"{"type":"thread.started","thread_id":"t1"}"#,
                r#"{"type":"item.completed","item":{"id":"item_0","type":"error","message":"config notice"}}"#,
                r#"{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"ls -la"}}"#,
                r#"{"type":"item.completed","item":{"id":"item_2","type":"agent_message","text":"Hello"}}"#,
                r#"{"type":"item.completed","item":{"id":"item_3","type":"agent_message","text":" world"}}"#,
                r#"{"type":"turn.completed","usage":{"input_tokens":7,"output_tokens":3}}"#,
            ],
        );
        assert_eq!(st.final_text, "Hello world");
        assert!(st.finished, "turn.completed 是终态");
        assert_eq!((st.input_tokens, st.output_tokens), (Some(7), Some(3)));
        assert_eq!(st.cost_usd, None);
        assert_eq!(deltas(&out), ["Hello", " world"]);
        assert!(matches!(&out[0], TurnEvent::Session(id) if id == "t1"));
        assert!(matches!(&out[1], TurnEvent::Notice(t) if t == "config notice"));
        assert!(matches!(&out[2], TurnEvent::Notice(t) if t == "运行命令：ls -la"));
    }

    #[test]
    fn pi_sums_usage_per_message() {
        let (st, out) = run(
            "pi",
            &[
                r#"{"type":"session","id":"p1"}"#,
                r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"Hi"}}"#,
                r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"thinking","thinking":"x"},{"type":"text","text":"Hi"}],"usage":{"input":100,"output":5,"cost":{"total":0.25}}}}"#,
                r#"{"type":"tool_execution_start","toolName":"bash"}"#,
                r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"!"}],"usage":{"input":1,"output":1,"cost":{"total":0.125}}}}"#,
            ],
        );
        assert_eq!(st.final_text, "Hi!");
        assert_eq!(st.cost_usd, Some(0.375));
        assert_eq!((st.input_tokens, st.output_tokens), (Some(101), Some(6)));
        assert_eq!(deltas(&out), ["Hi"]);
        // message_end 里的完整文本不再重复推送，只补上没有增量的部分
        assert!(
            out.iter()
                .any(|e| matches!(e, TurnEvent::Notice(t) if t == "调用工具 bash"))
        );
    }

    /// 只有轮级错误才是致命的
    #[test]
    fn turn_level_errors_are_fatal() {
        let (_, out) = run(
            "claude-cli",
            &[r#"{"type":"result","is_error":true,"result":"boom"}"#],
        );
        assert!(matches!(&out[0], TurnEvent::Failed(m) if m == "boom"));

        let (_, out) = run(
            "codex-cli",
            &[r#"{"type":"turn.failed","error":{"message":"no auth"}}"#],
        );
        assert!(matches!(&out[0], TurnEvent::Failed(m) if m == "no auth"));

        let (_, out) = run("pi", &[r#"{"type":"error","message":"bad model"}"#]);
        assert!(matches!(&out[0], TurnEvent::Failed(m) if m == "bad model"));
    }

    /// 各 Agent 的续聊与模型参数
    #[test]
    fn command_shapes() {
        let bin = Path::new("/bin/agent");
        let work = Path::new("/tmp/w");
        let spec = |adapter: &'static str, model, resume| TurnSpec {
            adapter,
            bin,
            prompt: "hi",
            model,
            resume,
            workdir: work,
        };
        let args = |cmd: &Command| -> Vec<String> {
            cmd.as_std()
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };

        let claude = build_command(&spec("claude-cli", Some("m"), Some("s")));
        assert_eq!(
            args(&claude),
            [
                "-p",
                "hi",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--model",
                "m",
                "--resume",
                "s",
            ]
        );

        assert_eq!(
            args(&build_command(&spec("codex-cli", None, None))),
            ["exec", "--json", "--skip-git-repo-check", "hi"]
        );
        assert_eq!(
            args(&build_command(&spec("codex-cli", Some("m"), Some("t1")))),
            [
                "exec",
                "resume",
                "--json",
                "--skip-git-repo-check",
                "-m",
                "m",
                "t1",
                "hi"
            ]
        );

        let grok = build_command(&spec("grok-cli", Some("m"), Some("g1")));
        assert_eq!(
            args(&grok),
            [
                "-p",
                "hi",
                "--output-format",
                "streaming-messages-json",
                "--include-partial-messages",
                "--verbatim",
                "-m",
                "m",
                "-r",
                "g1",
                "--cwd",
                "/tmp/w",
            ]
        );

        assert_eq!(
            args(&build_command(&spec("pi", Some("p/m"), Some("p1")))),
            [
                "-p",
                "hi",
                "--mode",
                "json",
                "--model",
                "p/m",
                "--session",
                "p1"
            ]
        );

        // 所有 Agent 都在指定工作目录下运行，并补齐 PATH
        for cmd in [&claude, &grok] {
            assert_eq!(cmd.as_std().get_current_dir(), Some(work));
            assert!(cmd.as_std().get_envs().any(|(k, _)| k == "PATH"));
        }
    }
}
