//! 测试共用的小工具：冒充 Agent 的脚本、收集推给界面的事件、组装测试用的应用状态。

use crate::chat::{ChatEvent, Sink};
use crate::remote::{NodeSink, NodeStatusEvent};
use crate::secrets::MemoryStore;
use crate::AppState;
use autoagents_core::runner::BinaryResolver;
use autoagents_core::service::{Service, ServiceOptions};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// 收集推给界面的事件
#[derive(Clone)]
pub struct Collect<T>(pub Arc<Mutex<Vec<T>>>);

impl<T> Default for Collect<T> {
    fn default() -> Self {
        Self(Arc::default())
    }
}

impl Sink for Collect<ChatEvent> {
    fn send(&self, event: ChatEvent) {
        self.0.lock().unwrap().push(event);
    }
}

impl NodeSink for Collect<NodeStatusEvent> {
    fn send(&self, event: NodeStatusEvent) {
        self.0.lock().unwrap().push(event);
    }
}

/// 写一个冒充某个 Agent 的脚本，返回它的路径
pub fn fake_script(agent: &str, script: &str) -> PathBuf {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join(agent);
    std::fs::write(&bin, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    std::mem::forget(dir);
    bin
}

/// 只认 `bin` 的机器侧服务；`name` 是它对外报的机器名
pub fn service_for(bin: PathBuf, name: Option<&str>) -> Arc<Service> {
    Service::new(ServiceOptions {
        name: name.map(str::to_string),
        resolve: Some(Arc::new(move |_: &str| Some(bin.clone())) as BinaryResolver),
        ..Default::default()
    })
}

/// 造一个冒充 claude 的脚本，并返回它的路径和只认它的服务
pub fn fake_agent(script: &str) -> (PathBuf, Arc<Service>) {
    let bin = fake_script("claude", script);
    let service = service_for(bin.clone(), None);
    (bin, service)
}

/// 测试用的应用状态，连同它收事件的两个收集器
pub struct TestState {
    pub state: Arc<AppState>,
    pub secrets: Arc<MemoryStore>,
    pub chat: Collect<ChatEvent>,
    pub nodes: Collect<NodeStatusEvent>,
}

/// 组装一份测试用的应用状态：内存数据库、内存凭据库
pub fn test_state(service: Arc<Service>) -> TestState {
    let chat = Collect::default();
    let nodes = Collect::default();
    let secrets = Arc::new(MemoryStore::default());
    let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
    let state = AppState::new(
        db,
        service,
        Arc::new(chat.clone()),
        Arc::new(nodes.clone()),
        secrets.clone(),
    );
    TestState {
        state,
        secrets,
        chat,
        nodes,
    }
}
// ---------------------------------------------------------------------------
// 假的 HTTP 服务：测试里冒充 GitHub、Vercel、Cloudflare 这样的外部接口
// ---------------------------------------------------------------------------

/// 一个解析好的 HTTP 请求：方法、路径（带查询串）、请求体，以及小写的请求头
pub struct Req {
    pub method: String,
    pub path: String,
    pub body: String,
    pub headers: HashMap<String, String>,
}

/// 极简的 HTTP 请求解析：读到请求头结束，再按 Content-Length 读请求体
pub async fn read_request(stream: &mut tokio::net::TcpStream) -> Req {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut chunk).await.unwrap();
        assert!(n > 0, "连接提前关闭");
        buf.extend_from_slice(&chunk[..n]);
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let length = head
        .lines()
        .find_map(|line| {
            let lower = line.to_ascii_lowercase();
            lower.strip_prefix("content-length:").map(str::to_string)
        })
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while buf.len() < header_end + length {
        let n = stream.read(&mut chunk).await.unwrap();
        assert!(n > 0, "连接提前关闭");
        buf.extend_from_slice(&chunk[..n]);
    }
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or_default().split_whitespace();
    let method = first.next().unwrap_or_default().to_string();
    let path = first.next().unwrap_or_default().to_string();
    let body = String::from_utf8_lossy(&buf[header_end..header_end + length]).into_owned();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    Req { method, path, body, headers }
}

/// 回一个响应，然后关掉连接
pub async fn respond(stream: &mut tokio::net::TcpStream, status: &str, content_type: &str, body: &str) {
    let text = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(text.as_bytes()).await;
}

// ---------------------------------------------------------------------------
// 造仓库：任务流水线与部署守护的测试里，Agent 要在一个真实的 Git 工作树里干活
// ---------------------------------------------------------------------------

pub fn git_in(dir: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=tester", "-c", "user.email=tester@example.com"])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("运行 git")
}

/// 造一个「远端在 GitHub 上」的仓库：裸仓库放在 …/github.com/o/r.git（流水线按 origin 地址里的 github.com 认仓库），
/// 再克隆一份当作机器上正在用的那份代码
pub fn setup_repo(root: &Path) -> PathBuf {
    setup_repo_at(root, "o", "r")
}

/// 同上，仓库是 `owner/name`
pub fn setup_repo_at(root: &Path, owner: &str, name: &str) -> PathBuf {
    let remote = root.join(format!("github.com/{owner}/{name}.git"));
    std::fs::create_dir_all(&remote).unwrap();
    assert!(git_in(&remote, &["init", "--bare", "-b", "main"]).status.success());
    let seed = root.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    assert!(git_in(&seed, &["init", "-b", "main"]).status.success());
    std::fs::write(seed.join("README.md"), "# demo\n").unwrap();
    assert!(git_in(&seed, &["add", "-A"]).status.success());
    assert!(git_in(&seed, &["commit", "-m", "init"]).status.success());
    assert!(git_in(&seed, &["remote", "add", "origin", remote.to_str().unwrap()]).status.success());
    assert!(git_in(&seed, &["push", "-q", "origin", "main"]).status.success());
    let machine = root.join("machine");
    std::fs::create_dir_all(&machine).unwrap();
    assert!(git_in(&machine, &["clone", "-q", remote.to_str().unwrap(), "app"]).status.success());
    machine.join("app")
}
