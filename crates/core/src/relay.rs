//! 自建中继：部署在有公网地址的机器上，让控制端与节点都只需要「出站」连接，
//! 工作电脑、家庭电脑在 NAT 后面也能互通。
//!
//! 中继只做三件事：验令牌、登记在线节点、在控制端与节点之间原样转发。
//! 握手之后转发的是端到端密文（见 [`crate::secure`]），中继看不到也改不了内容。
//!
//! ```text
//! 控制端 ──Hello(hub)──────────────▶ 中继 ◀──Hello(node, id)──────────── 节点(控制连接)
//!        ◀─Nodes[在线节点]──────────│
//!        ──Open(node_id)──────────▶ │ ──Incoming(conn_id)─────────────▶
//!        ◀─Opened───────────────────│ ◀──Join(conn_id)──────────────── 节点(数据连接)
//!        ◀════════════════ 原样转发（密文）════════════════════════════▶
//! ```

use crate::net::{self, Channel, Fingerprint, Stream};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_rustls::TlsAcceptor;

/// 节点应答 `Incoming` 的时限：过了就告诉控制端「节点没有响应」
const JOIN_WAIT: Duration = Duration::from_secs(20);

/// 中继上的连接多久没有消息就断开（两端都会定期发保活）
const IDLE: Duration = Duration::from_secs(150);

const TICK: Duration = Duration::from_secs(15);

/// 客户端重连中继的退避范围
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// 中继协议的消息
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RelayFrame {
    /// 连接进门先报到
    Hello {
        role: Role,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        os: Option<String>,
        token: String,
    },
    Welcome {},
    Error {
        message: String,
    },
    /// 在线节点列表（有变化就重发）
    Nodes {
        nodes: Vec<NodeSummary>,
    },
    /// 中继 → 节点的控制连接：有控制端要连你，请开一条数据连接 `Join`
    Incoming {
        conn_id: String,
    },
    /// 节点的数据连接：带着 `conn_id` 归队，之后与对应的控制端连接对拷
    Join {
        conn_id: String,
        token: String,
    },
    /// 控制端的数据连接：要连某个节点。成功后中继回 `Opened`，之后原样转发
    Open {
        node_id: String,
        token: String,
    },
    Opened {
        node_id: String,
    },
    Ping {},
    Pong {},
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Hub,
    Node,
}

/// 一台在线节点在中继上的登记信息
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeSummary {
    pub node_id: String,
    pub name: String,
    pub os: String,
    /// 连上来的时刻（Unix 毫秒）
    pub since_ms: u64,
}

/// 连接与元数据上限（防止内存/FD 耗尽 DoS）
const MAX_NODES: usize = 1024;
const MAX_HUBS: usize = 256;
const MAX_JOINS: usize = 1024;
const MAX_META_LEN: usize = 256;
const CTRL_QUEUE: usize = 64;
const CLIENT_IDLE: Duration = Duration::from_secs(90);

fn clip_meta(s: String) -> String {
    s.trim().chars().take(MAX_META_LEN).collect()
}

/// 一次节点登记。同一个节点 id 重新连上来会顶掉旧登记，旧连接下线时不能误删新的。
struct NodeEntry {
    seq: u64,
    summary: NodeSummary,
    tx: mpsc::Sender<RelayFrame>,
    /// 顶号时显式关闭旧控制连接，不等 IDLE 超时
    cancel: watch::Sender<bool>,
}

struct State<S> {
    /// 令牌只存哈希：内存里和转储里都不出现明文
    token_hash: [u8; 32],
    nodes: Mutex<HashMap<String, NodeEntry>>,
    hubs: Mutex<HashMap<u64, mpsc::Sender<RelayFrame>>>,
    /// 等待节点归队的数据连接
    joins: Mutex<HashMap<String, oneshot::Sender<Channel<S>>>>,
    online: watch::Sender<Vec<NodeSummary>>,
    seq: AtomicU64,
}

impl<S> State<S> {
    fn token_ok(&self, token: &str) -> bool {
        use subtle::ConstantTimeEq;
        let got: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        self.token_hash.ct_eq(&got).into()
    }

    fn summaries(&self) -> Vec<NodeSummary> {
        let mut list: Vec<NodeSummary> = self
            .nodes
            .lock()
            .unwrap()
            .values()
            .map(|entry| entry.summary.clone())
            .collect();
        list.sort_by(|a, b| a.node_id.cmp(&b.node_id));
        list
    }

    /// 在线节点有变化：更新列表并通知所有控制端（有界队列 `try_send`，慢消费者不拖垮中继内存）
    fn publish(&self) {
        let list = self.summaries();
        let _ = self.online.send(list.clone());
        let hubs: Vec<_> = self.hubs.lock().unwrap().values().cloned().collect();
        for hub in hubs {
            let _ = hub.try_send(RelayFrame::Nodes {
                nodes: list.clone(),
            });
        }
    }
}

/// 一个运行中的中继。`S` 是连接的底层流：自带 TLS 时是 TLS 流，放在反向代理后面时是 TCP 流。
pub struct Relay<S> {
    state: Arc<State<S>>,
}

/// 所有连接都是「TLS 流」的中继（自带证书）
pub type TlsRelay = Relay<net::Prefixed<tokio_rustls::server::TlsStream<TcpStream>>>;

/// 所有连接都是明文 TCP 的中继（TLS 由前面的反向代理终结）
pub type PlainRelay = Relay<net::Prefixed<TcpStream>>;

impl<S> Relay<S>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    pub fn new(token: &str, online: watch::Sender<Vec<NodeSummary>>) -> Arc<Self> {
        Arc::new(Self {
            state: Arc::new(State {
                token_hash: Sha256::digest(token.as_bytes()).into(),
                nodes: Mutex::new(HashMap::new()),
                hubs: Mutex::new(HashMap::new()),
                joins: Mutex::new(HashMap::new()),
                online,
                seq: AtomicU64::new(1),
            }),
        })
    }

    /// 处理一条已经完成 WebSocket 握手的连接，直到它结束。
    pub async fn accept(&self, channel: Channel<S>) -> Result<(), String> {
        let state = Arc::clone(&self.state);
        let mut channel = channel;
        let first = tokio::time::timeout(net::CONNECT_TIMEOUT, channel.recv_json::<RelayFrame>())
            .await
            .map_err(|_| "等待报到超时".to_string())?
            .map_err(|e| format!("没有收到报到：{e}"))?;
        match first {
            RelayFrame::Hello { token, .. } if !state.token_ok(&token) => {
                refuse(&mut channel, "令牌不对").await
            }
            RelayFrame::Hello {
                role: Role::Node,
                node_id,
                name,
                os,
                ..
            } => {
                let node_id = node_id
                    .map(clip_meta)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| "节点没有带 id".to_string())?;
                node_session(
                    &mut channel,
                    &state,
                    node_id,
                    clip_meta(name.unwrap_or_default()),
                    clip_meta(os.unwrap_or_default()),
                )
                .await
            }
            RelayFrame::Hello {
                role: Role::Hub, ..
            } => hub_session(&mut channel, &state).await,
            RelayFrame::Open { node_id, token } if state.token_ok(&token) => {
                open_session(channel, &state, clip_meta(node_id)).await
            }
            RelayFrame::Join { conn_id, token } if state.token_ok(&token) => {
                let waiting = state.joins.lock().unwrap().remove(&conn_id);
                match waiting {
                    // 交给等着的控制端连接，由它负责对拷
                    Some(waiting) => waiting
                        .send(channel)
                        .map_err(|_| "发起连接的控制端已经离开".to_string()),
                    None => refuse(&mut channel, "没有等待中的连接（可能已经超时）").await,
                }
            }
            RelayFrame::Open { .. } | RelayFrame::Join { .. } => {
                refuse(&mut channel, "令牌不对").await
            }
            _ => refuse(&mut channel, "应当先报到").await,
        }
    }
}

impl TlsRelay {
    /// 接受一个 TCP 连接：TLS → WebSocket 握手 → 处理。
    /// 普通的 HTTP 请求（健康检查）会得到 200 与 `health`，并返回 [`net::PLAIN_HTTP`]。
    pub async fn handle_tls(
        &self,
        stream: TcpStream,
        acceptor: TlsAcceptor,
        health: &str,
    ) -> Result<(), String> {
        let channel = tokio::time::timeout(
            net::CONNECT_TIMEOUT,
            net::accept_tls_with_health(stream, acceptor, health),
        )
        .await
        .map_err(|_| "TLS / WebSocket 握手超时".to_string())??;
        self.accept(channel).await
    }
}

impl PlainRelay {
    /// 接受一个明文 TCP 连接（TLS 已由反向代理终结）：WebSocket 握手 → 处理。
    pub async fn handle_plain(&self, stream: TcpStream, health: &str) -> Result<(), String> {
        let channel = tokio::time::timeout(
            net::CONNECT_TIMEOUT,
            net::accept_with_health(stream, health),
        )
        .await
        .map_err(|_| "WebSocket 握手超时".to_string())??;
        self.accept(channel).await
    }
}

fn log(message: &str) {
    eprintln!("[relay] {message}");
}

async fn refuse<S>(channel: &mut Channel<S>, message: &str) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let _ = channel
        .send_json(&RelayFrame::Error {
            message: message.to_string(),
        })
        .await;
    let _ = channel.close().await;
    Err(message.to_string())
}

/// 控制连接与已报到的控制端连接共用：把 `out` 里的消息写出去，
/// 读进来的只回应保活，超时或被顶替立即断开。
async fn control_loop<S>(
    channel: &mut Channel<S>,
    out: &mut mpsc::Receiver<RelayFrame>,
    cancel: &mut watch::Receiver<bool>,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_rx = tokio::time::Instant::now();
    loop {
        tokio::select! {
            frame = out.recv() => match frame {
                Some(frame) => channel.send_json(&frame).await?,
                None => return Err("连接已被本地结束".into()),
            },
            incoming = channel.recv_json::<RelayFrame>() => {
                if let RelayFrame::Ping {} = incoming? {
                    channel.send_json(&RelayFrame::Pong {}).await?;
                }
                last_rx = tokio::time::Instant::now();
            }
            _ = cancel.changed() => {
                let _ = channel
                    .send_json(&RelayFrame::Error {
                        message: "同一个节点 id 的新连接连了上来，这条连接被顶替".into(),
                    })
                    .await;
                let _ = channel.close().await;
                return Err("连接被新连接顶替".into());
            }
            _ = ticker.tick() => {
                let idle = last_rx.elapsed();
                if idle > IDLE {
                    return Err(format!("{} 秒没有消息，断开", idle.as_secs()));
                }
            }
        }
    }
}

struct NodeGuard<S> {
    state: Arc<State<S>>,
    node_id: String,
    seq: u64,
}

impl<S> Drop for NodeGuard<S> {
    fn drop(&mut self) {
        let mut nodes = self.state.nodes.lock().unwrap();
        if nodes.get(&self.node_id).is_some_and(|e| e.seq == self.seq) {
            nodes.remove(&self.node_id);
        }
        drop(nodes);
        self.state.publish();
    }
}

struct HubGuard<S> {
    state: Arc<State<S>>,
    id: u64,
}

impl<S> Drop for HubGuard<S> {
    fn drop(&mut self) {
        self.state.hubs.lock().unwrap().remove(&self.id);
    }
}

struct JoinGuard<S> {
    state: Arc<State<S>>,
    conn_id: String,
}

impl<S> Drop for JoinGuard<S> {
    fn drop(&mut self) {
        self.state.joins.lock().unwrap().remove(&self.conn_id);
    }
}

async fn node_session<S>(
    channel: &mut Channel<S>,
    state: &Arc<State<S>>,
    node_id: String,
    name: String,
    os: String,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (tx, mut rx) = mpsc::channel::<RelayFrame>(CTRL_QUEUE);
    let (cancel_tx, mut cancel_rx) = watch::channel(false);
    cancel_rx.borrow_and_update();
    let seq = state.seq.fetch_add(1, Ordering::SeqCst);
    let entry = NodeEntry {
        seq,
        summary: NodeSummary {
            node_id: node_id.clone(),
            name,
            os,
            since_ms: now_ms(),
        },
        tx,
        cancel: cancel_tx,
    };
    let full = {
        let mut nodes = state.nodes.lock().unwrap();
        if !nodes.contains_key(&node_id) && nodes.len() >= MAX_NODES {
            true
        } else {
            if let Some(old) = nodes.insert(node_id.clone(), entry) {
                // 同一个节点 id 连了两次：立即通知旧连接关闭
                let _ = old.cancel.send(true);
            }
            false
        }
    };
    if full {
        return refuse(channel, "中继已达到最大节点连接数").await;
    }
    let _guard = NodeGuard {
        state: Arc::clone(state),
        node_id: node_id.clone(),
        seq,
    };
    channel.send_json(&RelayFrame::Welcome {}).await?;
    log(&format!("节点上线：{}", node_id));
    state.publish();

    let result = control_loop(channel, &mut rx, &mut cancel_rx).await;
    log(&format!(
        "节点下线：{}（{}）",
        node_id,
        result
            .as_ref()
            .err()
            .map(String::as_str)
            .unwrap_or("对端关闭")
    ));
    result
}

async fn hub_session<S>(channel: &mut Channel<S>, state: &Arc<State<S>>) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let id = state.seq.fetch_add(1, Ordering::SeqCst);
    let (tx, mut rx) = mpsc::channel::<RelayFrame>(CTRL_QUEUE);
    let (_cancel_tx, mut cancel_rx) = watch::channel(false);
    cancel_rx.borrow_and_update();
    let full = {
        let mut hubs = state.hubs.lock().unwrap();
        if hubs.len() >= MAX_HUBS {
            true
        } else {
            hubs.insert(id, tx);
            false
        }
    };
    if full {
        return refuse(channel, "中继已达到最大控制端连接数").await;
    }
    let _guard = HubGuard {
        state: Arc::clone(state),
        id,
    };
    channel
        .send_json(&RelayFrame::Nodes {
            nodes: state.summaries(),
        })
        .await?;
    control_loop(channel, &mut rx, &mut cancel_rx).await
}

async fn open_session<S>(
    mut channel: Channel<S>,
    state: &Arc<State<S>>,
    node_id: String,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let Some(node) = state
        .nodes
        .lock()
        .unwrap()
        .get(&node_id)
        .map(|e| e.tx.clone())
    else {
        return refuse(&mut channel, &format!("节点 {node_id} 不在线")).await;
    };
    let conn_id = URL_SAFE_NO_PAD.encode(crate::secure::random::<16>());
    let (tx, rx) = oneshot::channel();
    let full = {
        let mut joins = state.joins.lock().unwrap();
        if joins.len() >= MAX_JOINS {
            true
        } else {
            joins.insert(conn_id.clone(), tx);
            false
        }
    };
    if full {
        return refuse(&mut channel, "中继等待队列已满，请稍后再试").await;
    }
    let _join_guard = JoinGuard {
        state: Arc::clone(state),
        conn_id: conn_id.clone(),
    };
    if node
        .try_send(RelayFrame::Incoming {
            conn_id: conn_id.clone(),
        })
        .is_err()
    {
        return refuse(&mut channel, &format!("节点 {node_id} 刚刚下线或过于繁忙")).await;
    }
    match tokio::time::timeout(JOIN_WAIT, rx).await {
        Ok(Ok(node_channel)) => {
            drop(_join_guard);
            channel
                .send_json(&RelayFrame::Opened {
                    node_id: node_id.clone(),
                })
                .await?;
            // 之后只是对拷：两端握手与之后的消息都是端到端密文，原样转发
            channel.pipe(node_channel).await
        }
        _ => {
            refuse(
                &mut channel,
                &format!("节点 {node_id} 没有响应（它的控制连接可能已经断了）"),
            )
            .await
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 客户端一侧（控制端与 agentd 共用）
// ---------------------------------------------------------------------------

/// 控制端：经中继连到某个节点。返回的连接还没握手，交给 [`crate::link::HubLink::handshake`]。
pub async fn open(
    relay_url: &str,
    pin: Option<&Fingerprint>,
    token: &str,
    node_id: &str,
) -> Result<Channel<Stream>, String> {
    let mut channel = net::dial(relay_url, pin).await?;
    tokio::time::timeout(
        net::CONNECT_TIMEOUT,
        channel.send_json(&RelayFrame::Open {
            node_id: node_id.to_string(),
            token: token.to_string(),
        }),
    )
    .await
    .map_err(|_| "向中继发起连接超时".to_string())??;
    let reply = tokio::time::timeout(
        net::CONNECT_TIMEOUT + JOIN_WAIT,
        channel.recv_json::<RelayFrame>(),
    )
    .await
    .map_err(|_| "等待中继响应超时".to_string())??;
    match reply {
        RelayFrame::Opened { .. } => Ok(channel),
        RelayFrame::Error { message } => Err(format!("中继：{message}")),
        other => Err(format!("中继应答不对：{other:?}")),
    }
}

/// 节点：为一次 `Incoming` 开一条数据连接并归队。返回的连接交给 [`crate::link::serve`]。
pub async fn join(
    relay_url: &str,
    pin: Option<&Fingerprint>,
    token: &str,
    conn_id: &str,
) -> Result<Channel<Stream>, String> {
    let mut channel = net::dial(relay_url, pin).await?;
    tokio::time::timeout(
        net::CONNECT_TIMEOUT,
        channel.send_json(&RelayFrame::Join {
            conn_id: conn_id.to_string(),
            token: token.to_string(),
        }),
    )
    .await
    .map_err(|_| "向中继归队超时".to_string())??;
    Ok(channel)
}

/// 节点：向中继报到（控制连接）。成功后中继经 `incoming` 送来 `Incoming`，
/// 节点应为每一个开一条数据连接 [`join`] 并交给 [`crate::link::serve`]。
/// 函数在连接结束时返回原因。
pub async fn node_control<F>(
    relay_url: &str,
    pin: Option<&Fingerprint>,
    token: &str,
    node_id: &str,
    name: &str,
    mut on_incoming: F,
) -> Result<(), String>
where
    F: FnMut(String) + Send,
{
    let mut channel = net::dial(relay_url, pin).await?;
    tokio::time::timeout(
        net::CONNECT_TIMEOUT,
        channel.send_json(&RelayFrame::Hello {
            role: Role::Node,
            node_id: Some(node_id.to_string()),
            name: Some(name.to_string()),
            os: Some(std::env::consts::OS.to_string()),
            token: token.to_string(),
        }),
    )
    .await
    .map_err(|_| "向中继报到超时".to_string())??;
    let welcome = tokio::time::timeout(net::CONNECT_TIMEOUT, channel.recv_json::<RelayFrame>())
        .await
        .map_err(|_| "等待中继欢迎消息超时".to_string())??;
    match welcome {
        RelayFrame::Welcome {} => {}
        RelayFrame::Error { message } => return Err(format!("中继：{message}")),
        other => return Err(format!("中继应答不对：{other:?}")),
    }
    let mut ticker = tokio::time::interval(net::PING_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_rx = tokio::time::Instant::now();
    loop {
        tokio::select! {
            incoming = channel.recv_json::<RelayFrame>() => {
                last_rx = tokio::time::Instant::now();
                match incoming? {
                    RelayFrame::Incoming { conn_id } => on_incoming(conn_id),
                    RelayFrame::Error { message } => return Err(format!("中继：{message}")),
                    _ => {}
                }
            }
            _ = ticker.tick() => {
                if last_rx.elapsed() > CLIENT_IDLE {
                    return Err(format!("中继 {} 秒未响应保活，断开重连", last_rx.elapsed().as_secs()));
                }
                channel.send_json(&RelayFrame::Ping {}).await?;
            }
        }
    }
}

/// 控制端与中继的连接状态
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayStatus {
    pub connected: bool,
    /// 最近一次断开或连接失败的原因
    pub message: Option<String>,
}

/// 控制端：与中继保持一条控制连接，拿在线节点列表。断线自动重连。
pub struct RelayWatcher {
    online: watch::Receiver<Vec<NodeSummary>>,
    status: watch::Receiver<RelayStatus>,
    shutdown: watch::Sender<bool>,
}

impl RelayWatcher {
    /// 在后台启动，立即返回。
    pub fn spawn(relay_url: impl Into<String>, pin: Option<Fingerprint>, token: String) -> Self {
        let (online, online_rx) = watch::channel(Vec::new());
        let (status, status_rx) = watch::channel(RelayStatus::default());
        let (shutdown, shutdown_rx) = watch::channel(false);
        tokio::spawn(watch_loop(
            relay_url.into(),
            pin,
            token,
            online,
            status,
            shutdown_rx,
        ));
        Self {
            online: online_rx,
            status: status_rx,
            shutdown,
        }
    }

    pub fn online(&self) -> watch::Receiver<Vec<NodeSummary>> {
        self.online.clone()
    }

    pub fn status(&self) -> RelayStatus {
        self.status.borrow().clone()
    }

    pub fn stop(&self) {
        let _ = self.shutdown.send(true);
    }
}

fn jitter(base: Duration) -> Duration {
    let byte = crate::secure::random::<1>()[0] as u64;
    // 0.75x ~ 1.25x 抖动，避免中继重启后所有客户端同时重连形成风暴
    let factor_milli = 750 + (byte * 500) / 255;
    Duration::from_millis((base.as_millis() as u64 * factor_milli) / 1000)
}

async fn watch_loop(
    url: String,
    pin: Option<Fingerprint>,
    token: String,
    online: watch::Sender<Vec<NodeSummary>>,
    status: watch::Sender<RelayStatus>,
    mut shutdown: watch::Receiver<bool>,
) {
    shutdown.borrow_and_update();
    let mut backoff = BACKOFF_MIN;
    loop {
        if *shutdown.borrow() {
            return;
        }
        let _ = status.send(RelayStatus {
            connected: false,
            message: Some("正在连接中继…".into()),
        });
        let started = tokio::time::Instant::now();
        let reason = tokio::select! {
            res = watch_once(&url, pin.as_ref(), &token, &online, &status) => match res {
                Ok(()) => "中继关闭了连接".to_string(),
                Err(e) => e,
            },
            _ = shutdown.changed() => {
                let _ = online.send(Vec::new());
                return;
            }
        };
        let _ = online.send(Vec::new());
        let _ = status.send(RelayStatus {
            connected: false,
            message: Some(reason),
        });
        // 连上过一小段时间才算稳定：偶发断开从头退避，一直连不上则拉长间隔
        if started.elapsed() > Duration::from_secs(30) {
            backoff = BACKOFF_MIN;
        }
        tokio::select! {
            _ = tokio::time::sleep(jitter(backoff)) => {}
            _ = shutdown.changed() => return,
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// 连一次中继：报到、收在线列表、定期保活。连接结束返回原因。
async fn watch_once(
    url: &str,
    pin: Option<&Fingerprint>,
    token: &str,
    online: &watch::Sender<Vec<NodeSummary>>,
    status: &watch::Sender<RelayStatus>,
) -> Result<(), String> {
    let mut channel = net::dial(url, pin).await?;
    tokio::time::timeout(
        net::CONNECT_TIMEOUT,
        channel.send_json(&RelayFrame::Hello {
            role: Role::Hub,
            node_id: None,
            name: None,
            os: None,
            token: token.to_string(),
        }),
    )
    .await
    .map_err(|_| "向中继报到超时".to_string())??;
    // 收到首帧在线列表才标记已连接，避免令牌错时 connected 闪烁为 true
    let first = tokio::time::timeout(net::CONNECT_TIMEOUT, channel.recv_json::<RelayFrame>())
        .await
        .map_err(|_| "等待中继响应超时".to_string())??;
    match first {
        RelayFrame::Nodes { nodes } => {
            let _ = online.send(nodes);
            let _ = status.send(RelayStatus {
                connected: true,
                message: None,
            });
        }
        RelayFrame::Error { message } => return Err(format!("中继：{message}")),
        other => return Err(format!("中继应答不对：{other:?}")),
    }
    let mut ticker = tokio::time::interval(net::PING_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_rx = tokio::time::Instant::now();
    loop {
        tokio::select! {
            incoming = channel.recv_json::<RelayFrame>() => {
                last_rx = tokio::time::Instant::now();
                match incoming? {
                    RelayFrame::Nodes { nodes } => {
                        let _ = online.send(nodes);
                    }
                    RelayFrame::Error { message } => return Err(format!("中继：{message}")),
                    RelayFrame::Welcome {} | RelayFrame::Pong {} => {}
                    _ => {}
                }
            }
            _ = ticker.tick() => {
                if last_rx.elapsed() > CLIENT_IDLE {
                    return Err(format!("中继 {} 秒未响应保活，断开重连", last_rx.elapsed().as_secs()));
                }
                channel.send_json(&RelayFrame::Ping {}).await?;
            }
        }
    }
}

/// 生成一个中继令牌（首次启动、没有指定令牌时）
pub fn generate_token() -> String {
    URL_SAFE_NO_PAD.encode(crate::secure::random::<24>())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::link::{self, HubLink, NodeAuth, NodeState};
    use crate::net::Identity;
    use crate::runner::BinaryResolver;
    use crate::service::{Service, ServiceOptions};
    use tokio::net::TcpListener;

    const TOKEN: &str = "test-relay-token";

    fn fake_agent(script: &str) -> Arc<Service> {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        std::fs::write(&bin, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        std::mem::forget(dir);
        Service::new(ServiceOptions {
            name: Some("home-pc".into()),
            resolve: Some(Arc::new(move |_: &str| Some(bin.clone())) as BinaryResolver),
            ..Default::default()
        })
    }

    struct TestRelay {
        url: String,
        fingerprint: Fingerprint,
        online: watch::Receiver<Vec<NodeSummary>>,
    }

    async fn start_relay() -> TestRelay {
        let dir = tempfile::tempdir().unwrap();
        let identity = Identity::load_or_create(dir.path()).unwrap();
        let acceptor = TlsAcceptor::from(identity.server_config().unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("wss://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let (tx, rx) = watch::channel(Vec::new());
        let relay: Arc<TlsRelay> = Relay::new(TOKEN, tx);
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let (relay, acceptor) = (Arc::clone(&relay), acceptor.clone());
                tokio::spawn(async move {
                    let _ = relay.handle_tls(stream, acceptor, "ok").await;
                });
            }
        });
        TestRelay {
            url,
            fingerprint: identity.fingerprint(),
            online: rx,
        }
    }

    fn bring_node_online(
        relay: &TestRelay,
        service: Arc<Service>,
        auth: Arc<NodeState>,
    ) -> tokio::task::JoinHandle<()> {
        let url = relay.url.clone();
        let fingerprint = relay.fingerprint.clone();
        // 整个节点就这一个任务：中止它，控制连接随之断开，节点下线
        tokio::spawn(async move {
            let _ = node_control(
                &url,
                Some(&fingerprint),
                TOKEN,
                "node-1",
                "home-pc",
                |conn_id| {
                    let (url, fingerprint, service, auth) = (
                        url.clone(),
                        fingerprint.clone(),
                        service.clone(),
                        auth.clone(),
                    );
                    tokio::spawn(async move {
                        match join(&url, Some(&fingerprint), TOKEN, &conn_id).await {
                            Ok(channel) => {
                                let _ = link::serve(channel, auth, service).await;
                            }
                            Err(e) => eprintln!("[test] 节点归队失败：{e}"),
                        }
                    });
                },
            )
            .await;
        })
    }

    async fn wait_online(relay: &TestRelay, present: bool) {
        let mut rx = relay.online.clone();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if rx.borrow().iter().any(|n| n.node_id == "node-1") == present {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "等待节点{}线超时",
                if present { "上" } else { "下" }
            );
            let _ = tokio::time::timeout(Duration::from_secs(5), rx.changed()).await;
        }
    }

    /// 全程走真实 TCP + TLS：节点在另一头，控制端经中继与它握手、发请求、收事件
    #[tokio::test]
    async fn end_to_end_through_the_relay() {
        let relay = start_relay().await;
        let service = fake_agent(
            r#"printf '%s\n' '{"type":"result","is_error":false,"result":"经中继的回答"}'"#,
        );
        let psk = crate::secure::random_psk();
        let auth = NodeState::new(
            "node-1".into(),
            [("hub-1".into(), psk)].into_iter().collect(),
            None,
        );
        let node = bring_node_online(&relay, Arc::clone(&service), Arc::clone(&auth));
        wait_online(&relay, true).await;

        // 中继的指纹不对就进不来
        assert!(
            open(&relay.url, Some(&Fingerprint([7; 32])), TOKEN, "node-1")
                .await
                .is_err()
        );

        let channel = tokio::time::timeout(
            Duration::from_secs(10),
            open(&relay.url, Some(&relay.fingerprint), TOKEN, "node-1"),
        )
        .await
        .expect("经中继连到节点")
        .unwrap();
        let link = tokio::time::timeout(
            Duration::from_secs(10),
            HubLink::handshake(channel, "hub-1", "node-1", psk, None),
        )
        .await
        .expect("经中继握手")
        .unwrap();
        let info = tokio::time::timeout(
            Duration::from_secs(10),
            link.call("sys.info", serde_json::json!({})),
        )
        .await
        .expect("经中继发请求")
        .unwrap();
        assert_eq!(info["name"], "home-pc");
        assert!(
            tokio::time::timeout(Duration::from_secs(10), link.ping())
                .await
                .expect("保活")
                .unwrap()
                < Duration::from_secs(5)
        );

        let mut events = link.subscribe();
        link.call(
            "turn.start",
            serde_json::json!({ "turnId": "3", "adapter": "claude-cli", "prompt": "hi" }),
        )
        .await
        .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let event = tokio::time::timeout_at(deadline, events.recv())
                .await
                .expect("等待对话事件")
                .unwrap();
            match event {
                crate::runner::TurnEvent::Finished { text, .. } => {
                    assert_eq!(text, "经中继的回答");
                    break;
                }
                crate::runner::TurnEvent::Failed { message, .. } => {
                    panic!("对话在节点上失败：{message}")
                }
                _ => {}
            }
        }
        assert!(link.closed_reason().is_none());
        link.close();
        node.abort();
        wait_online(&relay, false).await;
    }

    /// 令牌不对进不来；节点不在线时控制端拿到明确的原因
    #[tokio::test]
    async fn tokens_and_offline_nodes_are_reported() {
        let relay = start_relay().await;
        // 令牌错误：报到就被拒
        assert!(
            node_control(
                &relay.url,
                Some(&relay.fingerprint),
                "wrong",
                "node-1",
                "x",
                |_| {}
            )
            .await
            .unwrap_err()
            .contains("令牌不对")
        );
        let mut channel = net::dial(&relay.url, Some(&relay.fingerprint))
            .await
            .unwrap();
        channel
            .send_json(&RelayFrame::Hello {
                role: Role::Hub,
                node_id: None,
                name: None,
                os: None,
                token: "wrong".into(),
            })
            .await
            .unwrap();
        assert!(matches!(
            channel.recv_json::<RelayFrame>().await.unwrap(),
            RelayFrame::Error { message } if message.contains("令牌不对")
        ));
        // 没报到就想连节点
        let mut channel = net::dial(&relay.url, Some(&relay.fingerprint))
            .await
            .unwrap();
        channel.send_json(&RelayFrame::Ping {}).await.unwrap();
        assert!(matches!(
            channel.recv_json::<RelayFrame>().await.unwrap(),
            RelayFrame::Error { message } if message.contains("先报到")
        ));
        assert!(
            open(&relay.url, Some(&relay.fingerprint), "wrong", "node-1")
                .await
                .unwrap_err()
                .contains("令牌不对")
        );
        assert!(
            open(&relay.url, Some(&relay.fingerprint), TOKEN, "ghost")
                .await
                .unwrap_err()
                .contains("不在线")
        );
    }

    /// 控制端的监听：能看到节点上线、下线；中继不可达时给出原因，恢复后自动重连
    #[tokio::test]
    async fn watcher_tracks_online_nodes_and_reconnects() {
        let relay = start_relay().await;
        // 先启动一个连不上的地址，再让它变得可用：直接对监听循环验证重连
        let dead = RelayWatcher::spawn(
            "wss://127.0.0.1:1",
            Some(relay.fingerprint.clone()),
            TOKEN.into(),
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let status = dead.status();
            match status.message.as_deref() {
                Some(message) if !message.contains("正在连接") => {
                    assert!(!status.connected);
                    assert!(!message.is_empty(), "失败要带原因");
                    break;
                }
                _ => {}
            }
            assert!(tokio::time::Instant::now() < deadline, "没有报告连接失败");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        dead.stop();

        let watcher = RelayWatcher::spawn(
            relay.url.clone(),
            Some(relay.fingerprint.clone()),
            TOKEN.into(),
        );
        let service = fake_agent("sleep 30");
        let auth = NodeState::new("node-1".into(), HashMap::new(), None);
        let node = bring_node_online(&relay, service, auth);
        let mut online = watcher.online();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if online.borrow().len() == 1 {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "监听没有收到在线节点"
            );
            let _ = tokio::time::timeout(Duration::from_secs(5), online.changed()).await;
        }
        let node_info = online.borrow()[0].clone();
        assert_eq!(
            (node_info.node_id.as_str(), node_info.name.as_str()),
            ("node-1", "home-pc")
        );
        assert!(watcher.status().connected);
        node.abort();
        wait_online(&relay, false).await;
        watcher.stop();
    }

    /// 中继协议的线上格式
    #[test]
    fn relay_frames_round_trip() {
        let text = r#"{"type":"hello","role":"node","node_id":"n","name":"home","os":"macos","token":"t"}"#;
        let frame: RelayFrame = serde_json::from_str(text).unwrap();
        assert_eq!(
            frame,
            RelayFrame::Hello {
                role: Role::Node,
                node_id: Some("n".into()),
                name: Some("home".into()),
                os: Some("macos".into()),
                token: "t".into()
            }
        );
        assert_eq!(
            serde_json::to_string(&RelayFrame::Hello {
                role: Role::Hub,
                node_id: None,
                name: None,
                os: None,
                token: "t".into()
            })
            .unwrap(),
            r#"{"type":"hello","role":"hub","token":"t"}"#
        );
        let list = serde_json::to_string(&RelayFrame::Nodes {
            nodes: vec![NodeSummary {
                node_id: "n".into(),
                name: "home".into(),
                os: "macos".into(),
                since_ms: 5,
            }],
        })
        .unwrap();
        assert!(
            list.contains(r#""nodeId":"n""#) && list.contains(r#""sinceMs":5"#),
            "{list}"
        );
        assert_eq!(generate_token().len(), 32);
        assert_ne!(generate_token(), generate_token());
    }

    #[allow(dead_code)]
    fn _assert_node_auth_is_object_safe(_: Arc<dyn NodeAuth>) {}
}
