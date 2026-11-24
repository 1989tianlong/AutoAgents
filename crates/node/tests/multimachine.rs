//! 多机接入的真实联调：起真正的 autoagents-relay 与 agentd 进程，控制端这一头用桌面应用
//! 同样的代码（core 里的 relay / HubLink / secure）走完整条路：
//! 拿到中继连接串 → 机器凭配对码接入 → 握手并换成长期密钥 → 用长期密钥重连 → 转发请求
//! → agentd 重启后凭保存下来的密钥仍然连得上。

use autoagents_core::link::HubLink;
use autoagents_core::net::Fingerprint;
use autoagents_core::relay::{self, RelayWatcher};
use autoagents_core::secure::{self, Psk};
use serde_json::json;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const HUB: &str = "hub-e2e";

/// 起了一个子进程，并把它的标准错误收进内存（出错时好打出来看）
struct Process {
    child: Child,
    log: Arc<Mutex<String>>,
}

impl Process {
    fn spawn(mut command: Command, name: &str) -> Self {
        let mut child = command.stderr(Stdio::piped()).spawn().unwrap_or_else(|e| panic!("启动 {name}：{e}"));
        let log = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&log);
        let stderr = child.stderr.take().expect("标准错误");
        std::thread::spawn(move || {
            use std::io::Read;
            let mut text = String::new();
            let mut reader = stderr;
            // 一直读到管道关闭（进程退出）
            let _ = reader.read_to_string(&mut text);
            *sink.lock().unwrap() = text;
        });
        Self { child, log }
    }
}

/// 测试结束（包括断言失败）时把起的进程都收掉
struct Processes {
    procs: Vec<Process>,
    /// 中继的标准输出要一直有人拿着：管道一旦关掉，它再写日志就会被 SIGPIPE 结束
    _relay_stdout: Option<BufReader<std::process::ChildStdout>>,
}

impl Processes {
    fn dump(&self) -> String {
        let mut text = String::new();
        for (i, proc) in self.procs.iter().enumerate() {
            text.push_str(&format!("--- 进程 {i} 的标准错误（尾部）：\n{}\n", clip_tail(&proc.log.lock().unwrap(), 3000)));
        }
        text
    }
}

fn clip_tail(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        text.to_string()
    } else {
        chars[chars.len() - max_chars..].iter().collect()
    }
}

impl Drop for Processes {
    fn drop(&mut self) {
        for proc in &mut self.procs {
            let _ = proc.child.kill();
            let _ = proc.child.wait();
        }
    }
}

struct Relay {
    addr: String,
    token: String,
    pin: Fingerprint,
}

/// 起一个中继（端口由系统分配），等它打印连接串
fn start_relay(data_dir: &Path, processes: &mut Processes) -> Relay {
    let mut child = Process::spawn(
        {
            let mut command = Command::new(env!("CARGO_BIN_EXE_autoagents-relay"));
            command
                .args(["--listen", "127.0.0.1:0", "--data-dir"])
                .arg(data_dir)
                .stdout(Stdio::piped());
            command
        },
        "autoagents-relay",
    );
    let stdout = child.child.stdout.take().expect("中继的标准输出");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        assert!(
            reader.read_line(&mut line).expect("读中继输出") > 0,
            "中继提前退出了"
        );
        if let Some(text) = line.trim().strip_prefix("autoagents://relay?") {
            processes.procs.push(child);
            processes._relay_stdout = Some(reader);
            return parse_relay_string(text);
        }
    }
}

fn parse_relay_string(query: &str) -> Relay {
    let field = |key: &str| {
        query
            .split('&')
            .find_map(|pair| pair.strip_prefix(&format!("{key}=")))
            .unwrap_or_else(|| panic!("连接串里缺少 {key}：{query}"))
            .to_string()
    };
    Relay {
        addr: field("addr"),
        token: field("token"),
        pin: Fingerprint::parse(&field("fp")).expect("指纹"),
    }
}

/// 起一台「被管理的机器」
fn start_agentd(data_dir: &Path, relay: &Relay, code: Option<&str>, processes: &mut Processes) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agentd"));
    command
        .args(["--data-dir"])
        .arg(data_dir)
        .arg("--name")
        .arg("home-pc")
        .args(["--relay", &relay.addr, "--token", &relay.token, "--fp"])
        .arg(relay.pin.to_string())
        .stdout(Stdio::null());
    if let Some(code) = code {
        command.args(["--pair", code]);
    }
    processes.procs.push(Process::spawn(command, "agentd"));
}

async fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(40);
    while !check() {
        assert!(Instant::now() < deadline, "等待超时：{what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// 经中继连到节点并握手；`rotate_to` 是首次配对时换上的长期密钥
async fn connect(
    relay: &Relay,
    node_id: &str,
    psk: Psk,
    rotate_to: Option<Psk>,
) -> Result<HubLink, String> {
    let channel = relay::open(&relay.addr, Some(&relay.pin), &relay.token, node_id).await?;
    HubLink::handshake(channel, HUB, node_id, psk, rotate_to).await
}

/// 它刚回来时可能还没连好中继：多试几次
async fn wait_link(relay: &Relay, node_id: &str, psk: Psk) -> HubLink {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match connect(relay, node_id, psk, None).await {
            Ok(link) => return link,
            Err(e) => {
                assert!(Instant::now() < deadline, "一直连不上重启后的机器：{e}");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_machine_joins_through_a_real_relay_and_keeps_its_key_across_restarts() {
    let tmp = TempDir::new().unwrap();
    let mut processes = Processes {
        procs: Vec::new(),
        _relay_stdout: None,
    };
    let relay = start_relay(&tmp.path().join("relay"), &mut processes);
    assert!(
        relay.addr.starts_with("wss://127.0.0.1:"),
        "端口应由系统分配：{}",
        relay.addr
    );

    // 控制端这一头：跟踪中继上的在线机器
    let watcher = RelayWatcher::spawn(
        relay.addr.clone(),
        Some(relay.pin.clone()),
        relay.token.clone(),
    );
    wait_for("中继连上", || watcher.status().connected).await;
    assert!(watcher.online().borrow().is_empty(), "还没有机器接入");

    // 机器凭安装命令里的配对码启动
    let code = secure::pairing_code();
    let node_dir = tmp.path().join("node");
    start_agentd(&node_dir, &relay, Some(&code), &mut processes);
    wait_for("机器出现在中继上", || watcher.online().borrow().len() == 1).await;
    let node_id = watcher.online().borrow()[0].node_id.clone();
    assert_eq!(watcher.online().borrow()[0].name, "home-pc");

    // 配对码不对的连不上
    let long_term = secure::random_psk();
    let wrong = connect(
        &relay,
        &node_id,
        secure::psk_from_code("WRONG-CODE-WRONG-CODE"),
        Some(long_term),
    )
    .await;
    assert!(wrong.is_err(), "配对码不对不该配上");

    // 配对：握手后立即换成长期密钥
    let link = connect(
        &relay,
        &node_id,
        secure::psk_from_code(&code),
        Some(long_term),
    )
    .await
    .unwrap();
    let info = link.call("sys.info", json!({})).await.unwrap();
    assert_eq!(info["name"], "home-pc");
    assert_eq!(info["flavor"], "agentd");
    link.close();
    // 长期密钥落了盘，而且只有本机用户能读
    let hubs = node_dir.join("hubs.json");
    wait_for("配对信息落盘", || hubs.exists()).await;
    let saved = std::fs::read_to_string(&hubs).unwrap();
    assert!(saved.contains(HUB), "{saved}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&hubs).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "配对密钥文件不应当让其他用户读到");
    }
    // 配对码只能用一次
    let again = connect(
        &relay,
        &node_id,
        secure::psk_from_code(&code),
        Some(secure::random_psk()),
    )
    .await;
    assert!(again.is_err(), "配对码不该能重复使用");

    // 之后凭长期密钥连接：请求都转发得到
    let link = connect(&relay, &node_id, long_term, None)
        .await
        .unwrap_or_else(|e| panic!("配对之后凭长期密钥连不上：{e}\n{}", processes.dump()));
    let agents = link.call("agents.list", json!({})).await.unwrap();
    assert!(agents.is_array(), "{agents}");
    let listed = link
        .call("fs.ls", json!({ "path": tmp.path() }))
        .await
        .unwrap();
    // 机器侧返回的是规范化之后的路径（macOS 上 /var 是 /private/var 的符号链接）
    let canonical = tmp.path().canonicalize().unwrap();
    assert_eq!(listed["path"], json!(canonical.to_string_lossy()));
    assert!(link
        .call("nope", json!({}))
        .await
        .unwrap_err()
        .contains("不支持请求"));
    assert!(link.ping().await.is_ok());
    link.close();

    // agentd 重启（机器重启、服务更新）：不重新配对，凭保存的密钥照样连得上
    let mut agentd = processes.procs.pop().expect("agentd");
    let agentd = &mut agentd.child;
    agentd.kill().unwrap();
    agentd.wait().unwrap();
    wait_for("中继把下线的机器摘掉", || {
        watcher.online().borrow().is_empty()
    })
    .await;
    start_agentd(&node_dir, &relay, None, &mut processes);
    wait_for("机器重新上线", || watcher.online().borrow().len() == 1).await;
    assert_eq!(
        watcher.online().borrow()[0].node_id,
        node_id,
        "节点 id 要保持不变，否则控制端认不出它"
    );
    let link = wait_link(&relay, &node_id, long_term).await;
    let metrics = link.call("sys.metrics", json!({})).await.unwrap();
    assert!(metrics["cpuPercent"].is_number(), "{metrics}");
    link.close();

    // 别的密钥连不上：只有配对过的控制端能管这台机器
    let stranger = connect(&relay, &node_id, secure::random_psk(), None).await;
    assert!(stranger.is_err(), "没有配对的控制端不该连得上");
    watcher.stop();
}
