//! agentd：装在每台被管理的机器上的守护进程（工作电脑、家庭电脑、服务器）。
//!
//! 它把这台机器变成 AutoAgents 的一个节点：发现本机的 Agent、按控制端的指令执行对话、
//! 上报资源占用。控制端与本机之间的所有通信都是端到端加密的，中继看不到内容。
//!
//! 接入方式（二选一，也可以同时开）：
//! - 经中继（推荐，穿透 NAT）：`agentd --relay wss://中继地址 --token 令牌 [--fp 指纹]`
//! - 直连（本机有公网地址时）：`agentd --listen 0.0.0.0:7100`

use autoagents_core::link::{self, NodeAuth, NodeState};
use autoagents_core::net::{self, Fingerprint, Identity};
use autoagents_core::relay;
use autoagents_core::secure::{self, Psk};
use autoagents_core::service::{Service, ServiceOptions};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const SERVICE_LABEL: &str = "com.tianlong.autoagents.agentd";
/// 像 `println!`，但写不进去也不算错：输出被 `| head` 这类命令提前关掉时，
/// 安静地停下就好，不该让守护进程的启动输出变成一次崩溃。
///
/// 进程本身保持 Rust 的默认设置（忽略 SIGPIPE）：守护进程要往网络上写，
/// 对端突然断开时的一次写入失败只能是一次 `Err`，不能把整个进程带走。
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

/// 配对码默认有效期
const DEFAULT_PAIR_TTL: Duration = Duration::from_secs(600);
/// 重连中继的退避范围
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// 多久看一次有没有新的配对请求
const PAIR_POLL: Duration = Duration::from_secs(2);
/// 保存下来的配置（`<数据目录>/agentd.toml`）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Config {
    node_id: String,
    name: String,
    relay: String,
    token: String,
    fingerprint: String,
    listen: String,
    allow_roots: Vec<String>,
    max_turns: usize,
}

/// `agentd pair` 写下、守护进程读取的一次配对请求
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairRequest {
    code: String,
    until_ms: u64,
}

#[derive(Debug, Default)]
struct Options {
    data_dir: Option<PathBuf>,
    name: Option<String>,
    relay: Option<String>,
    token: Option<String>,
    fingerprint: Option<String>,
    listen: Option<String>,
    /// 首次接入时由 AutoAgents 生成的配对码
    pair: Option<String>,
    allow: Vec<String>,
    max_turns: Option<usize>,
    ttl: Option<Duration>,
}

enum Command {
    Run,
    Install,
    Uninstall,
    Pair,
    Info,
}

#[tokio::main]
async fn main() {
    let (command, options) = match parse() {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let result = match command {
        Command::Run => run(options).await,
        Command::Install => install(options).await,
        Command::Uninstall => uninstall(),
        Command::Pair => pair(options),
        Command::Info => info(options),
    };
    if let Err(message) = result {
        eprintln!("agentd：{message}");
        std::process::exit(1);
    }
}

fn usage() -> String {
    format!(
        "agentd {VERSION} — AutoAgents 节点守护进程\n\
         \n用法：agentd [命令] [选项]\n\
         \n命令：\n\
         \x20（无）        前台运行；给出的选项会保存，下次直接运行即可\n\
         \x20 install     安装为开机自启的服务并启动\n\
         \x20 uninstall   停止并移除服务\n\
         \x20 pair        开放配对：打印配对码，供另一台电脑上的 AutoAgents 接入本机\n\
         \x20 info        查看本节点的配置与状态\n\
         \n选项：\n\
         \x20 --relay <wss://…>      经中继接入（穿透 NAT，推荐）\n\
         \x20 --token <令牌>         中继令牌\n\
         \x20 --fp <sha256:…>        中继的自签名证书指纹（中继放在正规证书后面时不用填）\n\
         \x20 --pair <配对码>         首次接入：AutoAgents 为这台机器生成的配对码（只能用一次）\n\
         \x20 --listen <地址:端口>   直连模式：在本机监听，等待控制端直连\n\
         \x20 --name <名字>          在 AutoAgents 里显示的名字，默认用主机名\n\
         \x20 --allow <目录>         只允许操作这个目录之下（可重复给出）；不给则不限制\n\
         \x20 --max-turns <数量>     同时运行的对话上限，默认 8\n\
         \x20 --data-dir <目录>      配置与密钥的保存位置，默认 ~/.autoagents\n\
         \x20 --help                 显示这份说明\n\
         \x20 --version              显示版本"
    )
}

fn parse() -> Result<(Command, Options), String> {
    let mut options = Options::default();
    let mut command = Command::Run;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| -> Result<String, String> {
            args.next()
                .filter(|v| !v.starts_with("--"))
                .ok_or_else(|| format!("{name} 后面要跟一个值\n\n{}", usage()))
        };
        match arg.as_str() {
            "--help" | "-h" => {
                say!("{}", usage());
                std::process::exit(0);
            }
            "--version" | "-V" => {
                say!("agentd {VERSION}");
                std::process::exit(0);
            }
            "install" => command = Command::Install,
            "uninstall" => command = Command::Uninstall,
            "pair" => command = Command::Pair,
            "info" => command = Command::Info,
            "run" => command = Command::Run,
            "--data-dir" => options.data_dir = Some(PathBuf::from(value("--data-dir")?)),
            "--name" => options.name = Some(value("--name")?),
            "--relay" => options.relay = Some(value("--relay")?),
            "--token" => options.token = Some(value("--token")?),
            "--fp" | "--fingerprint" => options.fingerprint = Some(value("--fp")?),
            "--pair" => options.pair = Some(value("--pair")?),
            "--listen" => options.listen = Some(value("--listen")?),
            "--allow" => options.allow.push(value("--allow")?),
            "--max-turns" => {
                options.max_turns = Some(
                    value("--max-turns")?
                        .parse()
                        .map_err(|_| "--max-turns 后面要跟一个数字".to_string())?,
                )
            }
            "--ttl" => {
                let seconds: u64 = value("--ttl")?
                    .parse()
                    .map_err(|_| "--ttl 后面要跟秒数".to_string())?;
                options.ttl = Some(Duration::from_secs(seconds));
            }
            other => return Err(format!("不认识的参数「{other}」\n\n{}", usage())),
        }
    }
    Ok((command, options))
}

fn default_data_dir() -> PathBuf {
    autoagents_core::discovery::home_dir().join(".autoagents")
}

fn data_dir(options: &Options) -> PathBuf {
    options.data_dir.clone().unwrap_or_else(default_data_dir)
}

fn config_path(data_dir: &Path) -> PathBuf {
    data_dir.join("agentd.toml")
}

fn hubs_path(data_dir: &Path) -> PathBuf {
    data_dir.join("hubs.json")
}

fn pair_request_path(data_dir: &Path) -> PathBuf {
    data_dir.join("pair.request")
}

/// 读配置；命令行上给了值的字段覆盖进去并保存。
/// 配置文件损坏或无权限读取时显式报错，绝不静默生成新 node_id 覆盖原有配置。
fn load_config(options: &Options, data_dir: &Path) -> Result<Config, String> {
    let path = config_path(data_dir);
    let (mut config, mut changed) = match std::fs::read_to_string(&path) {
        Ok(text) => {
            let parsed: Config = toml::from_str(&text)
                .map_err(|e| format!("配置文件 {} 格式有误：{e}", path.display()))?;
            (parsed, false)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Config::default(), true),
        Err(e) => return Err(format!("读取配置 {} 失败：{e}", path.display())),
    };
    if config.node_id.trim().is_empty() {
        config.node_id = URL_SAFE_NO_PAD.encode(secure::random::<16>());
        changed = true;
    }
    if config.name.trim().is_empty() {
        config.name = autoagents_core::discovery::hostname();
        changed = true;
    }
    macro_rules! set {
        ($field:ident) => {
            if let Some($field) = options.$field.clone() {
                if config.$field != $field {
                    config.$field = $field;
                    changed = true;
                }
            }
        };
    }
    set!(name);
    set!(relay);
    set!(token);
    set!(fingerprint);
    set!(listen);
    set!(max_turns);
    if !options.allow.is_empty() && config.allow_roots != options.allow {
        config.allow_roots = options.allow.clone();
        changed = true;
    }
    if config.token.is_empty()
        && let Ok(env_token) = std::env::var("AUTOAGENTS_TOKEN")
        && !env_token.trim().is_empty()
    {
        config.token = env_token.trim().to_string();
    }
    if !config.relay.is_empty()
        && !(config.relay.starts_with("wss://") || config.relay.starts_with("ws://"))
    {
        return Err(format!("--relay 应以 wss:// 开头：{}", config.relay));
    }
    if !config.fingerprint.is_empty() {
        Fingerprint::parse(&config.fingerprint)?;
    }
    if changed {
        save_config(&config, data_dir)?;
    }
    Ok(config)
}

fn save_config(config: &Config, data_dir: &Path) -> Result<(), String> {
    let text = toml::to_string_pretty(config).map_err(|e| e.to_string())?;
    net::write_private(&config_path(data_dir), text.as_bytes())
}

fn load_hubs(data_dir: &Path) -> Result<HashMap<String, Psk>, String> {
    let path = hubs_path(data_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(format!("读取 {} 失败：{e}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(HashMap::new());
    }
    let raw: HashMap<String, String> =
        serde_json::from_str(&text).map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
    raw.into_iter()
        .map(|(hub, key)| {
            let bytes = URL_SAFE_NO_PAD
                .decode(key)
                .map_err(|_| format!("控制端 {hub} 的密钥编码不合法"))?;
            let psk: Psk = bytes
                .try_into()
                .map_err(|_| format!("控制端 {hub} 的密钥长度不对"))?;
            Ok((hub, psk))
        })
        .collect()
}

fn save_hub(data_dir: &Path, hub_id: &str, psk: Psk) -> Result<(), String> {
    let mut hubs = load_hubs(data_dir)?;
    hubs.insert(hub_id.to_string(), psk);
    let mut raw: HashMap<String, String> = hubs
        .into_iter()
        .map(|(hub, psk)| (hub, URL_SAFE_NO_PAD.encode(psk)))
        .collect();
    raw.retain(|_, key| !key.is_empty());
    let text = serde_json::to_string_pretty(&raw).map_err(|e| e.to_string())?;
    net::write_private(&hubs_path(data_dir), text.as_bytes())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 带本地时间的日志（守护进程的标准输出会进服务的日志文件）
fn log(message: &str) {
    eprintln!("[agentd {}] {message}", local_clock());
}

fn local_clock() -> String {
    #[cfg(unix)]
    {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as libc::time_t)
            .unwrap_or(0);
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: localtime_r 只写入调用方给的 tm，线程安全
        let ok = unsafe { libc::localtime_r(&secs, &mut tm) };
        if !ok.is_null() {
            return format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec);
        }
    }
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "{:02}:{:02}:{:02}Z",
        secs / 3600 % 24,
        secs / 60 % 60,
        secs % 60
    )
}

/// 前台运行
async fn run(options: Options) -> Result<(), String> {
    let data_dir = data_dir(&options);
    let config = load_config(&options, &data_dir)?;
    if config.relay.is_empty() && config.listen.is_empty() {
        return Err(format!(
            "还没有配置接入方式。用 AutoAgents 生成的安装命令，或者：\n  \
             agentd --relay wss://中继地址 --token 令牌 --fp 指纹 --pair 配对码\n  \
             agentd --listen 0.0.0.0:7100\n\n{}",
            usage()
        ));
    }
    let mut paired = load_hubs(&data_dir)?;
    let hubs_file = hubs_path(&data_dir);
    let state = NodeState::new(
        config.node_id.clone(),
        std::mem::take(&mut paired),
        Some(Box::new(move |hub_id, psk| {
            if let Err(e) = save_hub(hubs_file.parent().unwrap_or(Path::new(".")), hub_id, psk) {
                log(&format!("保存配对信息失败：{e}"));
            }
        })),
    );
    let pair_code = options.pair.clone().or_else(|| {
        std::env::var("AUTOAGENTS_PAIR_CODE")
            .ok()
            .filter(|s| !s.trim().is_empty())
    });
    if let Some(code) = &pair_code {
        secure::normalize_pairing_code(code)?;
        state.allow_pairing(code);
        log("已按安装命令开放配对，等待 AutoAgents 接入（10 分钟内有效）");
    }
    let service = Service::new(ServiceOptions {
        name: Some(config.name.clone()),
        flavor: "agentd",
        allowed_roots: config.allow_roots.iter().map(PathBuf::from).collect(),
        max_turns: config.max_turns,
        ..Default::default()
    });

    // 直连监听在后台任务启动前就完成绑定与证书加载，端口占用等启动错误立即失败退出
    let direct_listener = if !config.listen.is_empty() {
        let identity = Identity::load_or_create(&data_dir.join("identity"))
            .map_err(|e| format!("无法加载本机证书：{e}"))?;
        let acceptor = tokio_rustls::TlsAcceptor::from(
            identity
                .server_config()
                .map_err(|e| format!("本机证书不可用：{e}"))?,
        );
        let listener = tokio::net::TcpListener::bind(&config.listen)
            .await
            .map_err(|e| format!("无法监听 {}：{e}", config.listen))?;
        log(&format!(
            "直连模式：wss://{}，证书指纹 {}",
            config.listen,
            identity.fingerprint()
        ));
        Some((listener, acceptor))
    } else {
        None
    };

    say!(
        "AutoAgents agentd {VERSION}\n  节点：{}（id {}）\n  数据目录：{}",
        config.name,
        config.node_id,
        data_dir.display()
    );
    if config.allow_roots.is_empty() {
        say!("  允许操作的目录：不限制");
    } else {
        say!("  允许操作的目录：{}", config.allow_roots.join("，"));
    }

    let (shutdown, shutdown_rx) = watch::channel(false);
    spawn_signal_handler(shutdown.clone());
    tokio::spawn(pair_poller(
        pair_request_path(&data_dir),
        Arc::clone(&state),
        shutdown_rx.clone(),
    ));

    if let Some((listener, acceptor)) = direct_listener {
        tokio::spawn(listen_loop(
            listener,
            acceptor,
            Arc::clone(&state),
            Arc::clone(&service),
            shutdown_rx.clone(),
        ));
    }
    if !config.relay.is_empty() {
        tokio::spawn(relay_loop(
            config.clone(),
            Arc::clone(&state),
            Arc::clone(&service),
            shutdown_rx.clone(),
        ));
    }
    let mut done = shutdown_rx;
    done.borrow_and_update();
    let _ = done.changed().await;
    log("正在停止：结束本机正在运行的对话…");
    service.stop_all();
    tokio::time::sleep(Duration::from_millis(500)).await;
    log("已退出");
    Ok(())
}

fn spawn_signal_handler(shutdown: watch::Sender<bool>) {
    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("订阅 SIGTERM");
            tokio::select! {
                _ = ctrl_c => {}
                _ = term.recv() => {}
            }
        }
        #[cfg(not(unix))]
        let _ = ctrl_c.await;
        let _ = shutdown.send(true);
    });
}

/// 经中继接入：保持一条控制连接，断了就重连；每个 Incoming 开一条数据连接
async fn relay_loop(
    config: Config,
    state: Arc<NodeState>,
    service: Arc<Service>,
    mut shutdown: watch::Receiver<bool>,
) {
    shutdown.borrow_and_update();
    let pin = match Fingerprint::parse(&config.fingerprint) {
        Ok(pin) if !config.fingerprint.is_empty() => Some(pin),
        _ => None,
    };
    let mut backoff = BACKOFF_MIN;
    loop {
        let started = Instant::now();
        log(&format!("正在连接中继 {}", config.relay));
        let (relay_url, token, node_id, name) = (
            config.relay.clone(),
            config.token.clone(),
            config.node_id.clone(),
            config.name.clone(),
        );
        let join_limit = Arc::new(tokio::sync::Semaphore::new(64));
        // 每来一个控制端连接，闭包里都要有一份自己的副本
        let ctx = (
            relay_url.clone(),
            pin.clone(),
            token.clone(),
            Arc::clone(&state),
            Arc::clone(&service),
            join_limit,
        );
        let result = tokio::select! {
            res = relay::node_control(&relay_url, pin.as_ref(), &token, &node_id, &name, move |conn_id| {
                let (relay_url, pin, token, state, service, join_limit) = ctx.clone();
                let Ok(permit) = join_limit.try_acquire_owned() else {
                    log("并发数据连接已达上限，忽略本次接入");
                    return;
                };
                tokio::spawn(async move {
                    let _permit = permit;
                    match relay::join(&relay_url, pin.as_ref(), &token, &conn_id).await {
                        Ok(channel) => {
                            if let Err(e) = link::serve(channel, state, service).await {
                                log(&format!("一条控制端连接结束：{e}"));
                            }
                        }
                        Err(e) => log(&format!("接通控制端失败：{e}")),
                    }
                });
            }) => res,
            _ = shutdown.changed() => return,
        };
        let reason = result.err().unwrap_or_else(|| "中继关闭了连接".into());
        if started.elapsed() > Duration::from_secs(30) {
            backoff = BACKOFF_MIN;
        }
        if let Some(hint) = hint_for(&reason) {
            log(hint);
        }
        log(&format!(
            "与中继的连接断开：{reason}；{} 秒后重连",
            backoff.as_secs()
        ));
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = shutdown.changed() => return,
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// 连不上中继时，把最常见的几种原因直接说清楚
fn hint_for(reason: &str) -> Option<&'static str> {
    if reason.contains("UnknownIssuer") {
        Some(
            "  提示：中继用的是自签名证书时，必须用 --fp 指定它的证书指纹 \
             （中继启动时打印的「中继连接串」里有）",
        )
    } else if reason.contains("指纹不匹配") {
        Some("  提示：中继的证书变了（重新安装或重新生成过），请用新的指纹更新 --fp")
    } else if reason.contains("令牌不对") {
        Some("  提示：中继令牌不正确，请核对 --token 与中继启动时打印的令牌")
    } else if reason.contains("无法连接") || reason.contains("超时") {
        Some(
            "  提示：连不上中继。请检查地址与端口、中继是否在运行、服务器防火墙 / 安全组是否放行该端口",
        )
    } else {
        None
    }
}

/// 直连模式：在本机监听 TLS，等待控制端直连
async fn listen_loop(
    listener: tokio::net::TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    state: Arc<NodeState>,
    service: Arc<Service>,
    mut shutdown: watch::Receiver<bool>,
) {
    shutdown.borrow_and_update();
    let health = format!("autoagents agentd {VERSION}\n");
    let conn_limit = Arc::new(tokio::sync::Semaphore::new(64));
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = match accepted {
                    Ok(pair) => pair,
                    Err(e) => {
                        log(&format!("接受直连失败（退避重试）：{e}"));
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let Ok(permit) = Arc::clone(&conn_limit).try_acquire_owned() else {
                    log("直连并发数已达上限，拒绝新连接");
                    drop(stream);
                    continue;
                };
                let (acceptor, state, service) = (acceptor.clone(), Arc::clone(&state), Arc::clone(&service));
                let health = health.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    match net::accept_tls_with_health(stream, acceptor, &health).await {
                        Ok(channel) => {
                            if let Err(e) = link::serve(channel, state, service).await {
                                log(&format!("一条控制端连接结束：{e}"));
                            }
                        }
                        Err(e) if e != net::PLAIN_HTTP => log(&format!("握手失败：{e}")),
                        Err(_) => {}
                    }
                });
            }
            _ = shutdown.changed() => return,
        }
    }
}

/// 定时看有没有 `agentd pair` 留下的配对请求
async fn pair_poller(path: PathBuf, state: Arc<NodeState>, mut shutdown: watch::Receiver<bool>) {
    shutdown.borrow_and_update();
    loop {
        if let Ok(text) = tokio::fs::read_to_string(&path).await {
            let _ = tokio::fs::remove_file(&path).await;
            match serde_json::from_str::<PairRequest>(&text) {
                Ok(request) if request.until_ms > now_ms() => {
                    let remain_ms = request.until_ms.saturating_sub(now_ms());
                    let ttl = Duration::from_millis(remain_ms);
                    state.allow_pairing_for(&request.code, ttl);
                    let minutes = ttl.as_secs().div_ceil(60);
                    log(&format!("已开放配对：{minutes} 分钟内有效，只能用一次"));
                }
                Ok(_) => log("配对请求已经过期，已忽略"),
                Err(_) => log("配对请求无法解析，已忽略"),
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(PAIR_POLL) => {}
            _ = shutdown.changed() => return,
        }
    }
}

/// `agentd pair`：开放配对，打印配对码与接入所需的信息
fn pair(options: Options) -> Result<(), String> {
    let data_dir = data_dir(&options);
    let config = load_config(&options, &data_dir)?;
    let code = secure::pairing_code();
    let ttl = options.ttl.unwrap_or(DEFAULT_PAIR_TTL);
    let request = PairRequest {
        code: code.clone(),
        until_ms: now_ms() + ttl.as_millis() as u64,
    };
    net::write_private(
        &pair_request_path(&data_dir),
        serde_json::to_string(&request)
            .map_err(|e| e.to_string())?
            .as_bytes(),
    )?;
    say!(
        "配对码：{code}\n  {} 分钟内有效，只能用一次（守护进程会在几秒内读到这个请求）。\n",
        ttl.as_secs().div_ceil(60)
    );
    say!("在另一台电脑的 AutoAgents 里：机器 → 添加机器 → 选「已安装 agentd」。");
    if !config.relay.is_empty() {
        say!(
            "  在线机器里选「{}」，输入上面的配对码。\n  （选不到时手动填写节点 id：{}）",
            config.name,
            config.node_id
        );
    }
    if !config.listen.is_empty() {
        let fingerprint = Identity::load_or_create(&data_dir.join("identity"))
            .map_err(|e| e.to_string())?
            .fingerprint();
        let host = config
            .listen
            .rsplit_once(':')
            .map(|(host, port)| {
                if host == "0.0.0.0" || host.is_empty() {
                    format!("<本机公网地址>:{port}")
                } else {
                    format!("{host}:{port}")
                }
            })
            .unwrap_or_else(|| config.listen.clone());
        say!(
            "  直连地址：wss://{host}\n  证书指纹：{fingerprint}\n  或把这一行原样粘贴进去：\n  {}",
            node_connect_string(&host, &fingerprint, &config.node_id, &config.name, &code)
        );
    }
    Ok(())
}

/// 直连模式下贴进 AutoAgents 的连接串：地址、指纹、节点 id、配对码都在里面
fn node_connect_string(
    host: &str,
    fingerprint: &Fingerprint,
    node_id: &str,
    name: &str,
    code: &str,
) -> String {
    format!(
        "autoagents://node?addr=wss://{host}&fp={fingerprint}&id={node_id}&name={}&pair={code}",
        url_encode(name)
    )
}

/// 只处理连接串里会出现的字符（名字里的空格、中文等）
fn url_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// `agentd info`
fn info(options: Options) -> Result<(), String> {
    let data_dir = data_dir(&options);
    let config = load_config(&options, &data_dir)?;
    let hubs = load_hubs(&data_dir)?;
    say!("AutoAgents agentd {VERSION}");
    say!("  节点：{}（id {}）", config.name, config.node_id);
    say!("  数据目录：{}", data_dir.display());
    if config.relay.is_empty() && config.listen.is_empty() {
        say!("  接入方式：尚未配置");
    }
    if !config.relay.is_empty() {
        say!(
            "  经中继：{}{}",
            config.relay,
            if config.fingerprint.is_empty() {
                String::new()
            } else {
                format!("（证书指纹 {}）", short(&config.fingerprint))
            }
        );
    }
    if !config.listen.is_empty() {
        say!("  直连监听：{}", config.listen);
    }
    say!("  已配对的控制端：{} 个", hubs.len());
    say!(
        "  允许操作的目录：{}",
        if config.allow_roots.is_empty() {
            "不限制".to_string()
        } else {
            config.allow_roots.join("，")
        }
    );
    say!(
        "  同时运行的对话上限：{}",
        if config.max_turns == 0 {
            8
        } else {
            config.max_turns
        }
    );
    Ok(())
}

fn short(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= 20 {
        text.to_string()
    } else {
        format!("{}…", chars[..20].iter().collect::<String>())
    }
}

/// `agentd install`：保存配置，注册为开机自启的服务并启动
async fn install(options: Options) -> Result<(), String> {
    let data_dir = data_dir(&options);
    let config = load_config(&options, &data_dir)?;
    if config.relay.is_empty() && config.listen.is_empty() {
        return Err("安装前要先给出接入方式（--relay 或 --listen）".into());
    }
    let exe = std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok())
        .ok_or("找不到 agentd 自己的路径")?;
    let exe = exe.to_string_lossy().into_owned();
    let data = data_dir.to_string_lossy().into_owned();
    say!("节点：{}（id {}）", config.name, config.node_id);
    say!("程序：{exe}");
    say!("数据目录：{data}");

    if exe.chars().any(char::is_control) || data.chars().any(char::is_control) {
        return Err("路径不能包含换行或控制字符".into());
    }
    #[cfg(target_os = "macos")]
    {
        let xml_escape = |s: &str| -> String {
            s.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
                .replace('\'', "&apos;")
        };
        let home = autoagents_core::discovery::home_dir();
        let dir = home.join("Library/LaunchAgents");
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建 {} 失败：{e}", dir.display()))?;
        let plist = dir.join(format!("{SERVICE_LABEL}.plist"));
        let log_file = xml_escape(&data_dir.join("agentd.log").to_string_lossy());
        let exe_xml = xml_escape(&exe);
        let data_xml = xml_escape(&data);
        let content = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{SERVICE_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe_xml}</string>
        <string>--data-dir</string>
        <string>{data_xml}</string>
    </array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>{log_file}</string>
    <key>StandardErrorPath</key><string>{log_file}</string>
</dict>
</plist>
"#
        );
        std::fs::write(&plist, content)
            .map_err(|e| format!("写入 {} 失败：{e}", plist.display()))?;
        let uid = unsafe { libc::getuid() };
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", &format!("gui/{uid}/{SERVICE_LABEL}")])
            .output();
        let out = std::process::Command::new("launchctl")
            .args(["bootstrap", &format!("gui/{uid}"), &plist.to_string_lossy()])
            .output()
            .map_err(|e| format!("启动 launchctl 失败：{e}"))?;
        let ok = out.status.success() || {
            // 老系统的写法
            std::process::Command::new("launchctl")
                .args(["load", "-w", &plist.to_string_lossy()])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !ok {
            return Err(format!(
                "launchctl 加载服务失败：{}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        say!("已安装为开机自启的服务并启动（{}）。", plist.display());
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // systemd ExecStart 转义：双引号包裹并转义 \ " % $，防止指令或占位符注入
        let quote = |text: &str| {
            let escaped = text
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('%', "%%")
                .replace('$', "$$");
            format!("\"{escaped}\"")
        };
        let exec = format!("{} --data-dir {}", quote(&exe), quote(&data));
        let run = |args: &[&str]| -> Result<(), String> {
            let out = std::process::Command::new("systemctl")
                .args(args)
                .output()
                .map_err(|e| format!("运行 systemctl 失败：{e}"))?;
            if out.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "systemctl {} 失败：{}",
                    args.join(" "),
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        };
        let has_systemd = Path::new("/run/systemd/system").exists();
        if unsafe { libc::getuid() } == 0 && has_systemd {
            // 服务器上常直接用 root：这时 `systemctl --user` 多半连不上会话总线，注册成系统服务
            let unit = Path::new("/etc/systemd/system/autoagents-agentd.service");
            std::fs::write(
                unit,
                format!(
                    "[Unit]\nDescription=AutoAgents agentd\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nExecStart={exec}\nRestart=always\nRestartSec=3\n\n[Install]\nWantedBy=multi-user.target\n"
                ),
            )
            .map_err(|e| format!("写入 {} 失败：{e}", unit.display()))?;
            run(&["daemon-reload"])?;
            run(&["enable", "--now", "autoagents-agentd.service"])?;
            say!("已安装为 systemd 系统服务并启动（{}）。", unit.display());
            say!("它以 root 身份运行；只让它在授权过的目录里干活的话，加上 --allow 重新安装。");
        } else if has_systemd {
            let home = autoagents_core::discovery::home_dir();
            let dir = home.join(".config/systemd/user");
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("创建 {} 失败：{e}", dir.display()))?;
            let unit = dir.join("autoagents-agentd.service");
            std::fs::write(
                &unit,
                format!(
                    "[Unit]\nDescription=AutoAgents agentd\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nExecStart={exec}\nRestart=always\nRestartSec=3\n\n[Install]\nWantedBy=default.target\n"
                ),
            )
            .map_err(|e| format!("写入 {} 失败：{e}", unit.display()))?;
            run(&["--user", "daemon-reload"])?;
            run(&["--user", "enable", "--now", "autoagents-agentd.service"])?;
            say!("已安装为 systemd 用户服务并启动（{}）。", unit.display());
            // 没有登录时也要保持运行；没有权限就算了，提示一句
            let user = autoagents_core::discovery::home_dir()
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !user.is_empty()
                && std::process::Command::new("loginctl")
                    .args(["enable-linger", &user])
                    .output()
                    .is_ok_and(|out| out.status.success())
            {
                say!("已允许它在没有登录时也保持运行。");
            } else {
                say!("要让它在你没有登录时也保持运行：sudo loginctl enable-linger $USER");
            }
        } else {
            say!(
                "这台机器上没有 systemd，没法注册成服务。请自行保持下面这条命令运行（写进启动脚本、容器入口或进程管理器）："
            );
            say!("  {exec}");
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (exe, data);
        say!(
            "Windows 暂不支持自动注册服务。请把下面这行加入「启动」文件夹或任务计划程序：\n  {exe} --data-dir {data}"
        );
    }
    Ok(())
}

/// `agentd uninstall`
fn uninstall() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let home = autoagents_core::discovery::home_dir();
        let plist = home
            .join("Library/LaunchAgents")
            .join(format!("{SERVICE_LABEL}.plist"));
        let uid = unsafe { libc::getuid() };
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", &format!("gui/{uid}/{SERVICE_LABEL}")])
            .output();
        let _ = std::fs::remove_file(&plist);
        say!("已停止并移除服务。配置保留在 ~/.autoagents，重新安装会继续沿用。");
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // root 身份装的是系统服务，否则是用户服务；两处都清一遍
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "--now", "autoagents-agentd.service"])
            .output();
        let _ = std::process::Command::new("systemctl")
            .args(["disable", "--now", "autoagents-agentd.service"])
            .output();
        for unit in [
            autoagents_core::discovery::home_dir()
                .join(".config/systemd/user/autoagents-agentd.service"),
            PathBuf::from("/etc/systemd/system/autoagents-agentd.service"),
        ] {
            let _ = std::fs::remove_file(unit);
        }
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .output();
        let _ = std::process::Command::new("systemctl")
            .args(["daemon-reload"])
            .output();
        say!("已停止并移除服务。配置保留在数据目录里，重新安装会继续沿用。");
    }
    #[cfg(not(unix))]
    {
        say!("Windows 上没有注册服务，无需卸载。");
    }
    Ok(())
}
