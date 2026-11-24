//! autoagents-relay：自建中继。部署在有公网地址的机器上（一台 VPS 就够），
//! 控制端（AutoAgents 桌面应用）与各台机器上的 agentd 都只需向它发起出站连接。
//!
//! 中继只验令牌、登记在线节点、在两端之间原样转发密文，看不到任何内容。
//!
//! ```text
//! autoagents-relay                                   # 监听 0.0.0.0:7000，自带 TLS（自签名证书，按指纹钉住）
//! autoagents-relay --public wss://1.2.3.4:7000
//! autoagents-relay --no-tls --listen 127.0.0.1:7000 # 放在 Caddy / Nginx 等反向代理后面
//! ```

use autoagents_core::net::{self, Fingerprint, Identity};
use autoagents_core::relay::{self, PlainRelay, Relay, TlsRelay};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::watch;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_LISTEN: &str = "0.0.0.0:7000";

struct Options {
    listen: String,
    /// 对外地址：用来生成贴进 AutoAgents 的连接串
    public: Option<String>,
    data_dir: PathBuf,
    token: Option<String>,
    tls: bool,
}

#[tokio::main]
async fn main() {
    // 输出被提前关掉时像其它命令行工具一样安静地退出
    #[cfg(unix)]
    // SAFETY: 只设置本进程的信号处理方式
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    if let Err(e) = run().await {
        eprintln!("autoagents-relay：{e}");
        std::process::exit(1);
    }
}

fn usage() -> String {
    format!(
        "autoagents-relay {VERSION} — AutoAgents 中继\n\
         \n用法：autoagents-relay [选项]\n\
         \n选项：\n\
         \x20 --listen <地址:端口>   监听地址，默认 {DEFAULT_LISTEN}\n\
         \x20 --public <wss://…>    对外地址（域名或公网 IP），用于生成连接串\n\
         \x20 --token <令牌>        节点与控制端接入中继用的令牌；不填则生成并保存\n\
         \x20 --no-tls              不自带 TLS（已放在 Caddy / Nginx 等反向代理后面）\n\
         \x20 --data-dir <目录>     保存证书与令牌的目录，默认 ~/.autoagents-relay\n\
         \x20 --help                显示这份说明"
    )
}

fn parse() -> Result<Options, String> {
    let mut options = Options {
        listen: DEFAULT_LISTEN.to_string(),
        public: None,
        data_dir: default_data_dir(),
        token: None,
        tls: true,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| -> Result<String, String> {
            args.next()
                .filter(|v| !v.starts_with("--"))
                .ok_or_else(|| format!("{name} 后面要跟一个值\n\n{}", usage()))
        };
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            "--version" | "-V" => {
                println!("autoagents-relay {VERSION}");
                std::process::exit(0);
            }
            "--listen" => options.listen = value("--listen")?,
            "--public" => options.public = Some(value("--public")?),
            "--token" => options.token = Some(value("--token")?),
            "--no-tls" => options.tls = false,
            "--data-dir" => options.data_dir = PathBuf::from(value("--data-dir")?),
            other => return Err(format!("不认识的参数「{other}」\n\n{}", usage())),
        }
    }
    if !options.listen.contains(':') {
        options.listen = format!("{}:7000", options.listen);
    }
    if let Some(public) = &options.public {
        if !(public.starts_with("wss://") || public.starts_with("ws://")) {
            return Err("--public 应以 wss://（或反向代理前的 ws://）开头".into());
        }
    }
    Ok(options)
}

fn default_data_dir() -> PathBuf {
    autoagents_core::discovery::home_dir().join(".autoagents-relay")
}

enum Kind {
    Tls {
        relay: Arc<TlsRelay>,
        acceptor: tokio_rustls::TlsAcceptor,
    },
    Plain(Arc<PlainRelay>),
}

async fn run() -> Result<(), String> {
    let options = parse()?;
    std::fs::create_dir_all(&options.data_dir)
        .map_err(|e| format!("创建数据目录 {} 失败：{e}", options.data_dir.display()))?;
    let token = match options.token {
        Some(token) => token,
        None => load_or_create_token(&options.data_dir.join("token"))?,
    };
    let listener = TcpListener::bind(&options.listen)
        .await
        .map_err(|e| format!("无法监听 {}：{e}", options.listen))?;
    let bound = listener
        .local_addr()
        .map_err(|e| format!("无法得知监听地址：{e}"))?;
    let (online, _) = watch::channel(Vec::new());

    let (kind, fingerprint) = if options.tls {
        let identity = Identity::load_or_create(&options.data_dir.join("identity"))?;
        let acceptor = tokio_rustls::TlsAcceptor::from(identity.server_config()?);
        (
            Kind::Tls {
                relay: Relay::new(&token, online),
                acceptor,
            },
            Some(identity.fingerprint()),
        )
    } else {
        (Kind::Plain(Relay::new(&token, online)), None)
    };
    // 对外地址：没给的话，自带 TLS 时按监听地址推一个（提醒换成公网 IP 或域名）；
    // 放在反向代理后面时只能由用户给
    let public = options.public.clone().unwrap_or_else(|| {
        if !options.tls {
            return "wss://<反向代理的域名>".to_string();
        }
        let ip = bound.ip();
        let host = if ip.is_unspecified() {
            "<公网IP或域名>".to_string()
        } else if ip.is_ipv6() {
            format!("[{ip}]")
        } else {
            ip.to_string()
        };
        format!("wss://{host}:{}", bound.port())
    });

    println!(
        "AutoAgents 中继 {VERSION} 已启动\n  监听：{bound}{}\n  令牌：{token}\n  数据目录：{}",
        if options.tls {
            "（自带 TLS）"
        } else {
            "（未加密，请确认前面有 HTTPS 反向代理）"
        },
        options.data_dir.display()
    );
    if let Some(fingerprint) = &fingerprint {
        println!("  证书指纹：{fingerprint}");
    }
    println!(
        "\n把下面这行「中继连接串」粘贴到 AutoAgents 的「机器 → 中继设置」：\n  {}",
        connect_string(&public, fingerprint.as_ref(), &token)
    );
    println!("\nCtrl+C 停止。");

    let result = serve(
        listener,
        kind,
        shutdown_signal(),
        format!("autoagents-relay {VERSION} is running\n"),
    )
    .await;
    if let Err(e) = &result {
        eprintln!("[relay] 中继停止：{e}");
    }
    result
}

/// Ctrl+C 或 SIGTERM（服务管理器停止服务）时触发
fn shutdown_signal() -> watch::Receiver<bool> {
    let (tx, rx) = watch::channel(false);
    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        {
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("订阅 SIGTERM");
            tokio::select! {
                _ = ctrl_c => {}
                _ = term.recv() => {}
            }
        }
        #[cfg(not(unix))]
        let _ = ctrl_c.await;
        let _ = tx.send(true);
    });
    rx
}

async fn serve(
    listener: TcpListener,
    kind: Kind,
    mut shutdown: watch::Receiver<bool>,
    health: String,
) -> Result<(), String> {
    shutdown.borrow_and_update();
    let health = Arc::new(health);
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _peer) = accepted.map_err(|e| format!("接受连接失败：{e}"))?;
                let task = match &kind {
                    Kind::Tls { relay, acceptor } => Task::Tls((Arc::clone(relay), acceptor.clone())),
                    Kind::Plain(relay) => Task::Plain(Arc::clone(relay)),
                };
                let health = Arc::clone(&health);
                tokio::spawn(async move {
                    let result = match task {
                        Task::Tls((relay, acceptor)) => relay.handle_tls(stream, acceptor, &health).await,
                        Task::Plain(relay) => relay.handle_plain(stream, &health).await,
                    };
                    // 普通的 HTTP 请求（健康检查）已经回过应答，不算错误
                    if let Err(e) = result {
                        if e != net::PLAIN_HTTP {
                            eprintln!("[relay] {e}");
                        }
                    }
                });
            }
            _ = shutdown.changed() => return Ok(()),
        }
    }
}

enum Task {
    Tls((Arc<TlsRelay>, tokio_rustls::TlsAcceptor)),
    Plain(Arc<PlainRelay>),
}

fn load_or_create_token(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(token) if !token.trim().is_empty() => Ok(token.trim().to_string()),
        _ => {
            let token = relay::generate_token();
            write_private(path, token.as_bytes())?;
            Ok(token)
        }
    }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("创建 {} 失败：{e}", path.display()))?;
    file.write_all(bytes)
        .map_err(|e| format!("写入 {} 失败：{e}", path.display()))
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("写入 {} 失败：{e}", path.display()))
}

/// 贴进 AutoAgents 的连接串：地址、证书指纹、令牌都在里面
fn connect_string(public: &str, fingerprint: Option<&Fingerprint>, token: &str) -> String {
    let mut text = format!("autoagents://relay?addr={public}&token={token}");
    if let Some(fingerprint) = fingerprint {
        text.push_str(&format!("&fp={fingerprint}"));
    }
    text
}
