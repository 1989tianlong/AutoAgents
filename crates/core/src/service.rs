//! 机器侧服务：控制端发给节点的所有请求都在这里落地执行。
//!
//! 桌面应用把它嵌在进程内当作「本机」节点，agentd 则把它放在加密通道的另一端，
//! 所以无论 Agent 跑在哪台机器上，控制端的调用方式完全一样：
//!
//! | 请求 | 返回 |
//! |---|---|
//! | `sys.info` | 主机信息（见 [`machine::MachineInfo`]） |
//! | `sys.metrics` | 此刻的资源占用（见 [`machine::Metrics`]） |
//! | `agents.list` | 本机 Agent 清单 |
//! | `models.list` `{adapter}` | 某个 Agent 在本机可选的模型 |
//! | `fs.ls` `{path}` | 目录下的子目录（远程选择工作目录用） |
//! | `git.run` `{workdir, args}` | 在指定目录执行 git |
//! | `turn.start` | 开始一轮对话，过程经 `events` 推送 |
//! | `turn.stop` `{turnId}` | 停止一轮对话 |

use crate::adapters;
use crate::discovery;
use crate::machine;
use crate::models;
use crate::runner::{self, TurnEvent, TurnParams};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

/// git 命令的超时：推送大仓库、拉取慢镜像时可能较慢
const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// `fs.ls` 最多返回的目录数
const LS_LIMIT: usize = 500;

/// 一台机器默认允许同时跑多少轮对话
const DEFAULT_MAX_TURNS: usize = 8;

#[derive(Clone, Default)]
pub struct ServiceOptions {
    /// 这台机器的显示名；留空用主机名
    pub name: Option<String>,
    /// 服务由谁提供：「桌面应用内置」或「agentd」
    pub flavor: &'static str,
    /// 允许操作的工作目录；为空表示不限制
    pub allowed_roots: Vec<PathBuf>,
    /// 同时运行的对话轮数上限
    pub max_turns: usize,
    /// Agent 可执行文件的查找方式（测试时用来替换成假 Agent）
    pub resolve: Option<runner::BinaryResolver>,
}

pub struct Service {
    opts: ServiceOptions,
    allowed_roots: Vec<PathBuf>,
    turns: Mutex<HashMap<String, runner::TurnHandle>>,
}

impl Service {
    pub fn new(opts: ServiceOptions) -> Arc<Self> {
        let allowed_roots = opts
            .allowed_roots
            .iter()
            .filter_map(|root| root.canonicalize().ok())
            .collect();
        Arc::new(Self {
            opts,
            allowed_roots,
            turns: Mutex::new(HashMap::new()),
        })
    }

    /// 正在运行的对话轮数
    pub fn running_turns(&self) -> usize {
        self.turns.lock().unwrap().len()
    }

    /// 目录是否在允许范围内（规范化之后比较，`..` 与符号链接绕不过去）
    fn check_path(&self, path: &Path) -> Result<PathBuf, String> {
        let canonical = path
            .canonicalize()
            .map_err(|e| format!("无法访问 {}：{e}", path.display()))?;
        if self.allowed_roots.is_empty()
            || self.allowed_roots.iter().any(|root| canonical.starts_with(root))
        {
            Ok(canonical)
        } else {
            Err(format!(
                "{} 不在这台机器允许的目录范围内",
                path.display()
            ))
        }
    }

    /// 执行一个请求。`events` 用来推送 `turn.start` 之后的对话过程；
    /// 它被丢弃（连接断开）时，正在运行的对话会被停止。
    pub async fn call(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        events: mpsc::Sender<TurnEvent>,
    ) -> Result<Value, String> {
        let text = |key: &str| -> Result<&str, String> {
            params[key]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("缺少参数 {key}"))
        };
        match method {
            "sys.info" => to_value(machine::info(self.opts.name.as_deref(), self.opts.flavor)?),
            "sys.metrics" => {
                let running = self.running_turns();
                to_value(machine::metrics(running).await?)
            }
            "agents.list" => to_value(discovery::discover().await),
            "models.list" => to_value(models::load_models(text("adapter")?).await?),
            "fs.ls" => {
                let path = self.check_path(Path::new(text("path")?))?;
                ls(&path)
            }
            "git.run" => {
                let workdir = self.check_path(Path::new(text("workdir")?))?;
                let args: Vec<String> = params["args"]
                    .as_array()
                    .ok_or("args 应当是字符串数组")?
                    .iter()
                    .map(|a| a.as_str().map(str::to_string).ok_or("args 应当是字符串数组"))
                    .collect::<Result<_, _>>()?;
                to_value(git(&workdir, &args).await?)
            }
            "turn.start" => {
                let params: TurnParams = serde_json::from_value(params.clone())
                    .map_err(|e| format!("turn.start 参数不合法：{e}"))?;
                self.start_turn(params, events)
            }
            "turn.stop" => {
                let id = text("turnId")?;
                let stopped = self.stop_turn(id);
                Ok(json!({ "stopped": stopped }))
            }
            _ => Err(format!("这台机器不支持请求「{method}」")),
        }
    }

    fn start_turn(
        self: &Arc<Self>,
        params: TurnParams,
        events: mpsc::Sender<TurnEvent>,
    ) -> Result<Value, String> {
        if params.turn_id.trim().is_empty() {
            return Err("缺少参数 turnId".into());
        }
        if params.prompt.trim().is_empty() {
            return Err("消息不能为空".into());
        }
        if !adapters::is_supported(&params.adapter) {
            return Err(format!("「{}」的会话适配尚未实现", params.adapter));
        }
        // 先校验再启动：目录按这台机器的规则展开（`~` 指这里的主目录），
        // 再看它是否落在允许范围内。启动之后失败就只能以失败事件告知了。
        let workdir = runner::resolve_workdir(params.workdir.as_deref())?;
        if !self.allowed_roots.is_empty() {
            self.check_path(&workdir)?;
        }
        let id = params.turn_id.clone();
        {
            let mut turns = self.turns.lock().unwrap();
            if turns.contains_key(&id) {
                return Err("该会话上一轮还在运行，请等它结束或先停止".into());
            }
            let max = if self.opts.max_turns == 0 {
                DEFAULT_MAX_TURNS
            } else {
                self.opts.max_turns
            };
            if turns.len() >= max {
                return Err(format!(
                    "这台机器上同时运行的对话已达上限（{max}），等其中一轮结束再试"
                ));
            }
            let output = match &self.opts.resolve {
                Some(resolve) => runner::start_with(params, resolve.clone()),
                None => runner::start(params),
            };
            turns.insert(id.clone(), output.handle.clone());
            let service = Arc::clone(self);
            let turn_id = id.clone();
            tokio::spawn(async move {
                let mut events_rx = output.events;
                let mut stopped = false;
                while let Some(event) = events_rx.recv().await {
                    let ended = matches!(event, TurnEvent::Finished { .. } | TurnEvent::Failed { .. });
                    if !stopped && events.send(event).await.is_err() {
                        // 调用方已经离开（连接断开）：不让 Agent 在这台机器上无人看管地跑下去
                        stopped = true;
                        output.handle.stop();
                    }
                    // 终态一出手就不算运行中了：控制端紧接着发下一轮时，这一轮不能还占着会话号
                    if ended {
                        service.turns.lock().unwrap().remove(&turn_id);
                    }
                }
                service.turns.lock().unwrap().remove(&turn_id);
            });
        }
        Ok(json!({ "turnId": id }))
    }

    /// 停止一轮对话；返回它是否在运行
    pub fn stop_turn(&self, turn_id: &str) -> bool {
        match self.turns.lock().unwrap().get(turn_id) {
            Some(handle) => {
                handle.stop();
                true
            }
            None => false,
        }
    }

    /// 停止所有正在运行的对话（agentd 退出前调用）
    pub fn stop_all(&self) {
        let handles: Vec<_> = self.turns.lock().unwrap().values().cloned().collect();
        for handle in handles {
            handle.stop();
        }
    }
}

fn to_value<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DirEntry {
    name: String,
    /// 是不是 git 仓库（便于在远程机器上找到项目）
    git: bool,
}

/// 列出目录下的子目录（不含隐藏目录），目录优先按名字排序
fn ls(path: &Path) -> Result<Value, String> {
    let entries = std::fs::read_dir(path)
        .map_err(|e| format!("无法读取 {}：{e}", path.display()))?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            entry.path().is_dir().then(|| DirEntry {
                git: entry.path().join(".git").exists(),
                name,
            })
        })
        .take(LS_LIMIT)
        .collect::<Vec<_>>();
    Ok(json!({
        "path": path.to_string_lossy(),
        "parent": path.parent().map(|p| p.to_string_lossy().into_owned()),
        "entries": entries,
    }))
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitOutput {
    /// 退出码；被信号结束时为空
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl GitOutput {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

/// 在 `workdir` 下执行 `git <args>`
pub async fn git(workdir: &Path, args: &[String]) -> Result<GitOutput, String> {
    if args.is_empty() {
        return Err("git 参数不能为空".into());
    }
    let output = tokio::time::timeout(
        GIT_TIMEOUT,
        tokio::process::Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(args)
            .env("PATH", discovery::child_path())
            .stdin(Stdio::null())
            .output(),
    )
    .await;
    match output {
        Err(_) => Err(format!("git 执行超时（{} 秒）", GIT_TIMEOUT.as_secs())),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Err("这台机器上没有安装 git".into()),
        Ok(Err(e)) => Err(format!("启动 git 失败：{e}")),
        Ok(Ok(out)) => Ok(GitOutput {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::runner::BinaryResolver;

    fn service(name: Option<&str>, roots: Vec<PathBuf>) -> Arc<Service> {
        Service::new(ServiceOptions {
            name: name.map(str::to_string),
            flavor: "test",
            allowed_roots: roots,
            ..Default::default()
        })
    }

    async fn call(service: &Arc<Service>, method: &str, params: Value) -> Result<Value, String> {
        let (tx, mut rx) = mpsc::channel(16);
        // 事件通道不能在请求返回前被丢掉，否则正在运行的对话会被当成「调用方已离开」
        tokio::spawn(async move {
            while rx.recv().await.is_some() {}
        });
        service.call(method, &params, tx).await
    }

    #[tokio::test]
    async fn info_and_metrics() {
        let s = service(Some("home-pc"), vec![]);
        let info = call(&s, "sys.info", json!({})).await.unwrap();
        assert_eq!(info["name"], "home-pc");
        assert_eq!(info["flavor"], "test");
        assert!(info["cpuCount"].as_u64().unwrap() > 0);
        let metrics = call(&s, "sys.metrics", json!({})).await.unwrap();
        assert_eq!(metrics["runningTurns"], 0);
        assert!(metrics["memTotal"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn unknown_method_and_missing_params() {
        let s = service(None, vec![]);
        assert!(call(&s, "nope", json!({})).await.is_err());
        assert!(call(&s, "fs.ls", json!({})).await.unwrap_err().contains("path"));
        assert!(call(&s, "models.list", json!({ "adapter": "zcode" })).await.is_err());
    }

    /// 允许范围：符号链接与 `..` 都绕不过去
    #[tokio::test]
    async fn allowed_roots_are_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let inside = dir.path().join("inside");
        let outside = dir.path().join("outside");
        std::fs::create_dir(&inside).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let s = service(None, vec![inside.clone()]);
        assert!(call(&s, "fs.ls", json!({ "path": inside })).await.is_ok());
        let err = call(&s, "fs.ls", json!({ "path": outside })).await.unwrap_err();
        assert!(err.contains("允许的目录范围"), "{err}");
        let sneaky = inside.join("../outside");
        let err = call(&s, "fs.ls", json!({ "path": sneaky })).await.unwrap_err();
        assert!(err.contains("允许的目录范围"), "{err}");
        // macOS 的临时目录本身是 /private 的符号链接，规范化后仍要落在范围内
        assert!(s.allowed_roots[0].is_absolute());
    }

    #[tokio::test]
    async fn ls_lists_dirs_and_marks_repos() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("project/.git")).unwrap();
        std::fs::create_dir(dir.path().join("plain")).unwrap();
        std::fs::create_dir(dir.path().join(".hidden")).unwrap();
        std::fs::write(dir.path().join("file.txt"), "x").unwrap();
        let s = service(None, vec![]);
        let v = call(&s, "fs.ls", json!({ "path": dir.path() })).await.unwrap();
        let entries = v["entries"].as_array().unwrap_or_else(|| panic!("意外的返回：{v}"));
        assert_eq!(entries.len(), 2, "只列目录，不含隐藏目录与文件");
        let names: Vec<&str> = entries.iter().map(|e| e["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"project") && names.contains(&"plain"));
        let project = entries.iter().find(|e| e["name"] == "project").unwrap();
        assert_eq!(project["git"], true);
        assert!(v["parent"].is_string());
    }

    #[tokio::test]
    async fn git_runs_in_workdir() {
        let dir = tempfile::tempdir().unwrap();
        let s = service(None, vec![]);
        let init = call(
            &s,
            "git.run",
            json!({ "workdir": dir.path(), "args": ["init", "-q"] }),
        )
        .await
        .unwrap();
        assert_eq!(init["code"], 0);
        let top = call(
            &s,
            "git.run",
            json!({ "workdir": dir.path(), "args": ["rev-parse", "--show-toplevel"] }),
        )
        .await
        .unwrap();
        assert_eq!(top["code"], 0);
        assert!(top["stdout"].as_str().unwrap().trim().ends_with(dir.path().file_name().unwrap().to_str().unwrap()));
        // 不在仓库里：返回非零退出码和 stderr，而不是报错
        let parent = dir.path().parent().unwrap();
        let out = call(
            &s,
            "git.run",
            json!({ "workdir": parent, "args": ["status"] }),
        )
        .await
        .unwrap();
        assert_ne!(out["code"], 0);
        assert!(call(&s, "git.run", json!({ "workdir": dir.path(), "args": [] })).await.is_err());
    }

    /// 完整走一遍：启动 → 收到事件 → 以 Finished 结束；重复的 turnId 被拒绝
    #[tokio::test]
    async fn turn_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        std::fs::write(
            &bin,
            r#"#!/bin/sh
printf '%s\n' '{"type":"system","subtype":"init","session_id":"s9"}' '{"type":"result","is_error":false,"result":"ok"}'
"#,
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let bin2 = bin.clone();
        let s = Service::new(ServiceOptions {
            resolve: Some(Arc::new(move |_: &str| Some(bin2.clone())) as BinaryResolver),
            ..Default::default()
        });
        let (tx, mut rx) = mpsc::channel(64);
        let params = json!({ "turnId": "42", "adapter": "claude-cli", "prompt": "hi", "workdir": dir.path() });
        let started = s.call("turn.start", &params, tx).await.unwrap();
        assert_eq!(started["turnId"], "42");
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(serde_json::to_value(&event).unwrap()["type"].as_str().unwrap().to_string());
        }
        // result 里的文本在本例中没有 assistant 消息承载，按整条增量带回
        assert_eq!(events, ["turn.session", "turn.delta", "turn.finished"]);

        // 参数校验
        let (tx, _rx) = mpsc::channel(8);
        assert!(s.call("turn.start", &json!({ "turnId": "1", "adapter": "claude-cli", "prompt": "  " }), tx).await.unwrap_err().contains("不能为空"));
        let (tx, _rx) = mpsc::channel(8);
        assert!(s.call("turn.start", &json!({ "turnId": "1", "adapter": "zcode", "prompt": "hi" }), tx).await.unwrap_err().contains("适配"));
        assert_eq!(s.running_turns(), 0);
    }

    /// 工作目录 `~` 与留空等价，都指这台机器的主目录
    #[tokio::test]
    async fn tilde_workdir_resolves_to_home() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let bin2 = bin.clone();
        let s = Service::new(ServiceOptions {
            resolve: Some(Arc::new(move |_: &str| Some(bin2.clone())) as BinaryResolver),
            allowed_roots: vec![discovery::home_dir()],
            ..Default::default()
        });
        let (tx, _keep) = mpsc::channel(8);
        for workdir in [json!("~"), json!(""), json!("~/"), Value::Null] {
            let params = json!({ "turnId": "t", "adapter": "claude-cli", "prompt": "hi", "workdir": workdir });
            s.call("turn.start", &params, tx.clone()).await.unwrap();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while s.running_turns() > 0 && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert_eq!(s.running_turns(), 0);
        }
    }

    /// 同一会话同一时间只跑一轮；停止后可以再发
    #[tokio::test]
    async fn turn_is_exclusive_and_stoppable() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        std::fs::write(&bin, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let bin2 = bin.clone();
        let s = Service::new(ServiceOptions {
            resolve: Some(Arc::new(move |_: &str| Some(bin2.clone())) as BinaryResolver),
            ..Default::default()
        });
        let params = json!({ "turnId": "7", "adapter": "claude-cli", "prompt": "hi" });
        let (tx, mut rx) = mpsc::channel(64);
        s.call("turn.start", &params, tx).await.unwrap();
        let (tx, _keep) = mpsc::channel(8);
        assert!(s
            .call("turn.start", &params, tx)
            .await
            .unwrap_err()
            .contains("还在运行"));
        assert_eq!(s.running_turns(), 1);
        assert!(s.stop_turn("7"));
        assert!(!s.stop_turn("nope"));
        let last = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(last, TurnEvent::Failed { .. }));
        // 收尾后从运行中摘掉
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while s.running_turns() > 0 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(s.running_turns(), 0);
    }

    /// 并发上限
    #[tokio::test]
    async fn max_turns_is_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        std::fs::write(&bin, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let bin2 = bin.clone();
        let s = Service::new(ServiceOptions {
            resolve: Some(Arc::new(move |_: &str| Some(bin2.clone())) as BinaryResolver),
            max_turns: 1,
            ..Default::default()
        });
        let (tx, _keep) = mpsc::channel(64);
        s.call("turn.start", &json!({ "turnId": "a", "adapter": "claude-cli", "prompt": "hi" }), tx)
            .await
            .unwrap();
        let (tx, _keep2) = mpsc::channel(64);
        assert!(s
            .call("turn.start", &json!({ "turnId": "b", "adapter": "claude-cli", "prompt": "hi" }), tx)
            .await
            .unwrap_err()
            .contains("上限"));
        s.stop_all();
    }
}
