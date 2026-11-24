//! 一条「控制端 ↔ 节点」连接的两端：
//!
//! - [`HubLink`]：控制端这一头。握手、发请求等应答、订阅对话事件、保活与断线感知
//! - [`serve`]：节点这一头。校验控制端身份、把请求交给机器侧 [`Service`]、
//!   把对话事件推回去；连接断开时连带停掉这条连接上启动的对话
//!
//! 两头都不关心连接是怎么建立的（直连或经中继），只要求拿到一条 [`Channel`]。

use crate::net::Channel;
use crate::protocol::{Frame, Wire};
use crate::runner::TurnEvent;
use crate::secure::{self, Psk, Responder};
use crate::service::Service;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

/// 控制端多久没收到任何消息就发一次保活
const PING_AFTER: Duration = Duration::from_secs(20);

/// 控制端多久没收到任何消息就认定连接已死
const DEAD_AFTER: Duration = Duration::from_secs(60);

/// 节点这一头多久没收到控制端的消息就断开（控制端失联、机器休眠）
const NODE_IDLE: Duration = Duration::from_secs(120);

/// 一般请求的等待时间
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// 发送队列容量
const QUEUE: usize = 256;

/// 事件广播容量
const EVENTS: usize = 4096;

const TICK: Duration = Duration::from_secs(5);

/// 配对码默认有效期
pub const PAIRING_TTL: Duration = Duration::from_secs(600);

/// 每条连接一个序号，用来给这条连接上的对话加前缀
static CONNECTION: AtomicU64 = AtomicU64::new(1);

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// 控制端这一头的连接。可以克隆，最后一份丢开时连接关闭。
#[derive(Clone)]
pub struct HubLink {
    out: mpsc::Sender<Wire>,
    pending: Pending,
    events: broadcast::Sender<TurnEvent>,
    closed: watch::Receiver<Option<String>>,
    shutdown: watch::Sender<bool>,
    next_id: Arc<AtomicU64>,
    pings: Arc<Mutex<HashMap<u64, Instant>>>,
    rtt_ms: Arc<Mutex<Option<u64>>>,
}

impl std::fmt::Debug for HubLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubLink")
            .field("closed", &self.closed_reason())
            .field("rtt_ms", &self.rtt_ms())
            .finish()
    }
}

/// 控制端这一头的收发状态，收发循环与对外接口共用
struct Io {
    pending: Pending,
    events: broadcast::Sender<TurnEvent>,
    pings: Arc<Mutex<HashMap<u64, Instant>>>,
    rtt_ms: Arc<Mutex<Option<u64>>>,
}

impl HubLink {
    /// 在一条已建好的连接上完成握手。
    ///
    /// `rotate_to`：首次配对时 `psk` 是由配对码派生的，握手完成后立即换成这个
    /// 新的长期密钥（调用方应先把新密钥存好，再来握手）。
    pub async fn handshake<S>(
        mut channel: Channel<S>,
        hub_id: &str,
        node_id: &str,
        psk: Psk,
        rotate_to: Option<Psk>,
    ) -> Result<Self, String>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let initiator = secure::Initiator::new(psk, hub_id, node_id);
        channel
            .send(&Wire::Frame(Frame::Hello(initiator.hello())))
            .await?;
        match channel.recv().await? {
            Wire::Frame(Frame::HelloOk(reply)) => {
                let (cipher, confirm) = match initiator.finish(&reply) {
                    Ok(ok) => ok,
                    Err(e) => {
                        reject(&mut channel, &e).await;
                        return Err(e);
                    }
                };
                // 确认本身仍是明文（节点验完它才会开始解密）；它只含 mac，不涉密。
                // 确认一发出，这个方向的消息就全是密文了
                channel
                    .send(&Wire::Frame(Frame::Confirm(confirm)))
                    .await?;
                channel.encrypt(cipher);
            }
            Wire::Frame(Frame::Reject { message }) => {
                return Err(format!("节点拒绝了连接：{message}"));
            }
            other => {
                let e = format!("握手应答不对：{other:?}");
                reject(&mut channel, &e).await;
                return Err(e);
            }
        }
        if let Some(fresh) = rotate_to {
            channel
                .send(&Wire::Frame(Frame::Rotate {
                    psk: URL_SAFE_NO_PAD.encode(fresh),
                }))
                .await?;
            match channel.recv().await? {
                Wire::Frame(Frame::Rotated {}) => {}
                other => {
                    let e = format!("节点没有确认新密钥：{other:?}");
                    reject(&mut channel, &e).await;
                    return Err(e);
                }
            }
        }
        let (out, out_rx) = mpsc::channel(QUEUE);
        let (events, _) = broadcast::channel(EVENTS);
        let (closed, closed_rx) = watch::channel(None);
        let (shutdown, mut shutdown_rx) = watch::channel(false);
        // 初始值不算「变化」：否则收发循环一启动就以为是主动断开
        shutdown_rx.borrow_and_update();
        let io = Arc::new(Io {
            pending: Arc::default(),
            events,
            pings: Arc::new(Mutex::new(HashMap::new())),
            rtt_ms: Arc::new(Mutex::new(None)),
        });
        tokio::spawn(run_io(channel, out_rx, io.clone(), closed, shutdown_rx));
        Ok(Self {
            out,
            pending: Arc::clone(&io.pending),
            events: io.events.clone(),
            closed: closed_rx,
            shutdown,
            next_id: Arc::new(AtomicU64::new(1)),
            pings: Arc::clone(&io.pings),
            rtt_ms: Arc::clone(&io.rtt_ms),
        })
    }

    /// 发一个请求并等结果。`turn.start` 在节点确认启动后即返回，
    /// 之后的过程事件经 [`HubLink::subscribe`] 推送。
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        self.call_timeout(method, params, CALL_TIMEOUT).await
    }

    pub async fn call_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if self
            .out
            .send(Wire::Frame(Frame::request(id, method, params)))
            .await
            .is_err()
        {
            self.pending.lock().unwrap().remove(&id);
            return Err(self.closed_reason().unwrap_or_else(|| "连接已断开".into()));
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(self.closed_reason().unwrap_or_else(|| "连接已断开".into())),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(format!(
                    "节点在 {} 秒内没有回应（{method}）",
                    timeout.as_secs()
                ))
            }
        }
    }

    /// 订阅这台机器推来的对话事件
    pub fn subscribe(&self) -> broadcast::Receiver<TurnEvent> {
        self.events.subscribe()
    }

    /// 最近一次保活的往返耗时（毫秒）
    pub fn rtt_ms(&self) -> Option<u64> {
        *self.rtt_ms.lock().unwrap()
    }

    /// 连接是否已经断开；断开时带上原因
    pub fn closed_reason(&self) -> Option<String> {
        self.closed.borrow().clone()
    }

    /// 等连接断开，返回原因
    pub async fn wait_closed(&self) -> String {
        let mut rx = self.closed.clone();
        loop {
            if let Some(reason) = rx.borrow().clone() {
                return reason;
            }
            if rx.changed().await.is_err() {
                return "连接已断开".into();
            }
        }
    }

    /// 主动断开
    pub fn close(&self) {
        let _ = self.shutdown.send(true);
    }

    /// 测一次往返耗时
    pub async fn ping(&self) -> Result<Duration, String> {
        let nonce = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pings.lock().unwrap().insert(nonce, Instant::now());
        self.pending.lock().unwrap().insert(nonce, tx);
        self.out
            .send(Wire::Frame(Frame::Ping { nonce }))
            .await
            .map_err(|_| self.closed_reason().unwrap_or_else(|| "连接已断开".into()))?;
        let reply = tokio::time::timeout(Duration::from_secs(10), rx).await;
        self.pings.lock().unwrap().remove(&nonce);
        match reply {
            Ok(Ok(Ok(_))) => Ok(Duration::from_millis(self.rtt_ms().unwrap_or(0))),
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(_)) => Err(self.closed_reason().unwrap_or_else(|| "连接已断开".into())),
            Err(_) => {
                self.pending.lock().unwrap().remove(&nonce);
                Err("节点 10 秒内没有回应保活".into())
            }
        }
    }
}

/// 告知对端握手被拒绝的原因，然后关闭
async fn reject<S: AsyncRead + AsyncWrite + Unpin>(channel: &mut Channel<S>, message: &str) {
    let _ = channel
        .send(&Wire::Frame(Frame::Reject {
            message: message.to_string(),
        }))
        .await;
    let _ = channel.close().await;
}

/// 控制端这一头的收发循环：写出请求，读入应答与事件，保活，感知断线。
async fn run_io<S>(
    mut channel: Channel<S>,
    mut out_rx: mpsc::Receiver<Wire>,
    io: Arc<Io>,
    closed: watch::Sender<Option<String>>,
    mut shutdown: watch::Receiver<bool>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_rx = Instant::now();
    let mut idle_pings = 0u64;
    let result: Result<(), String> = loop {
        tokio::select! {
            wire = out_rx.recv() => match wire {
                Some(wire) => {
                    if let Err(e) = channel.send(&wire).await {
                        break Err(e);
                    }
                }
                // 控制端已经丢开了这条连接
                None => break Err("控制端断开了连接".into()),
            },
            incoming = channel.recv() => {
                let wire = match incoming {
                    Ok(wire) => wire,
                    Err(e) => break Err(e),
                };
                last_rx = Instant::now();
                match wire {
                    Wire::Frame(Frame::Response { id, result, error }) => {
                        let reply = match (result, error) {
                            (Some(result), _) => Ok(result),
                            (_, Some(error)) => Err(error),
                            (None, None) => Ok(Value::Null),
                        };
                        if let Some(tx) = io.pending.lock().unwrap().remove(&id) {
                            let _ = tx.send(reply);
                        }
                    }
                    Wire::Frame(Frame::Pong { nonce }) => {
                        if let Some(sent) = io.pings.lock().unwrap().remove(&nonce) {
                            *io.rtt_ms.lock().unwrap() = Some(sent.elapsed().as_millis() as u64);
                        }
                        if let Some(tx) = io.pending.lock().unwrap().remove(&nonce) {
                            let _ = tx.send(Ok(Value::Null));
                        }
                    }
                    Wire::Event(event) => {
                        // 没有订阅者时广播会报错，属正常情况
                        let _ = io.events.send(event);
                    }
                    // Reject 只在握手阶段出现；之后收到说明对端状态不对
                    Wire::Frame(Frame::Reject { message }) => {
                        break Err(format!("节点拒绝了连接：{message}"));
                    }
                    _ => {}
                }
            }
            _ = ticker.tick() => {
                let idle = last_rx.elapsed();
                if idle > DEAD_AFTER {
                    break Err(format!("节点 {} 秒没有任何消息，判定为断线", idle.as_secs()));
                }
                if idle > PING_AFTER {
                    idle_pings = idle_pings.wrapping_add(1);
                    let nonce = u64::MAX - idle_pings;
                    io.pings.lock().unwrap().insert(nonce, Instant::now());
                    if channel.send(&Wire::Frame(Frame::Ping { nonce })).await.is_err() {
                        break Err("发送失败".into());
                    }
                }
            }
            _ = shutdown.changed() => break Err("控制端主动断开".into()),
        }
    };
    let reason = result.err().unwrap_or_default();
    // 把等着的请求全部结束掉，免得调用方一直等
    for (_, tx) in io.pending.lock().unwrap().drain() {
        let _ = tx.send(Err(reason.clone()));
    }
    let _ = closed.send(Some(reason.clone()));
    let _ = channel.close().await;
}

/// 节点这一头的身份与配对状态
pub trait NodeAuth: Send + Sync + 'static {
    fn node_id(&self) -> String;
    /// 已配对的控制端的长期密钥
    fn paired_psk(&self, hub_id: &str) -> Option<Psk>;
    /// 待配对的配对码派生的密钥。返回 `Some` 表示这次连接是「首次配对」
    fn pending_psk(&self) -> Option<Psk>;
    /// 配对完成：保存新的长期密钥，配对码作废
    fn paired(&self, hub_id: &str, psk: Psk);
    /// 已配对的控制端为另一台设备开放配对
    fn allow_pairing(&self, code: &str);
}

/// [`NodeAuth`] 的现成实现：内存里的配对表，`persist` 负责落盘。
pub struct NodeState {
    node_id: String,
    inner: Mutex<NodeInner>,
    persist: Option<Box<dyn Fn(&str, Psk) + Send + Sync>>,
}

struct NodeInner {
    paired: HashMap<String, Psk>,
    pending: Option<(Psk, Instant)>,
}

impl NodeState {
    /// `paired`：已配对的控制端（从磁盘读回来的）
    pub fn new(
        node_id: String,
        paired: HashMap<String, Psk>,
        persist: Option<Box<dyn Fn(&str, Psk) + Send + Sync>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            node_id,
            inner: Mutex::new(NodeInner {
                paired,
                pending: None,
            }),
            persist,
        })
    }

    /// 已配对的控制端个数
    pub fn paired_count(&self) -> usize {
        self.inner.lock().unwrap().paired.len()
    }

    /// 是否有等待使用的配对码
    pub fn is_pairing_open(&self) -> bool {
        self.inner
            .lock()
            .unwrap()
            .pending
            .as_ref()
            .is_some_and(|( _, until)| Instant::now() < *until)
    }
}

impl NodeAuth for NodeState {
    fn node_id(&self) -> String {
        self.node_id.clone()
    }

    fn paired_psk(&self, hub_id: &str) -> Option<Psk> {
        self.inner.lock().unwrap().paired.get(hub_id).copied()
    }

    fn pending_psk(&self) -> Option<Psk> {
        let mut inner = self.inner.lock().unwrap();
        match inner.pending {
            Some((psk, until)) if Instant::now() < until => Some(psk),
            Some(_) => {
                inner.pending = None;
                None
            }
            None => None,
        }
    }

    fn paired(&self, hub_id: &str, psk: Psk) {
        let mut inner = self.inner.lock().unwrap();
        inner.paired.insert(hub_id.to_string(), psk);
        // 配对码只能用一次
        inner.pending = None;
        if let Some(persist) = &self.persist {
            persist(hub_id, psk);
        }
    }

    fn allow_pairing(&self, code: &str) {
        self.inner.lock().unwrap().pending =
            Some((secure::psk_from_code(code), Instant::now() + PAIRING_TTL));
    }
}

/// 节点这一头：在一条已建好的连接上服务一个控制端，直到连接结束。
///
/// 返回 `Err` 的原因只用于日志。连接断了（或者这个任务被整个丢弃）就会停掉
/// 这条连接上启动的对话，不让 Agent 在这台机器上无人看管地跑下去。
pub async fn serve<S>(
    mut channel: Channel<S>,
    auth: Arc<dyn NodeAuth>,
    service: Arc<Service>,
) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let guard = TurnGuard::new(Arc::clone(&service));
    let result = serve_inner(&mut channel, auth, Arc::clone(&service), Arc::clone(&guard.turns)).await;
    let _ = channel.close().await;
    // guard 在这里离开作用域：停掉这条连接上还在跑的对话
    result
}

/// 连接上启动过的对话（供连接结束时统一停掉）
type TurnSet = Arc<Mutex<std::collections::HashSet<String>>>;

/// 这条连接上启动过的对话的清单。离开作用域时停掉它们——正常结束、出错、
/// 任务被整个丢弃都算。对话可能正安静地等着模型响应、没有事件可推，
/// 光靠「推送失败」发现不了断线。
struct TurnGuard {
    turns: TurnSet,
    service: Arc<Service>,
}

impl TurnGuard {
    fn new(service: Arc<Service>) -> Self {
        Self {
            turns: Arc::default(),
            service,
        }
    }
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        for key in self.turns.lock().unwrap().iter() {
            self.service.stop_turn(key);
        }
    }
}

async fn serve_inner<S>(
    channel: &mut Channel<S>,
    auth: Arc<dyn NodeAuth>,
    service: Arc<Service>,
    turns: TurnSet,
) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // ---- 握手 ----
    let (hub_id, pairing) = match channel.recv().await? {
        Wire::Frame(Frame::Hello(hello)) => {
            if hello.node_id != auth.node_id() {
                let e = format!("找错节点了：这里是 {}", auth.node_id());
                reject(channel, &e).await;
                return Err(e);
            }
            let (psk, pairing) = match auth.paired_psk(&hello.hub_id) {
                Some(psk) => (psk, false),
                None => match auth.pending_psk() {
                    Some(psk) => (psk, true),
                    None => {
                        let e = "这台机器还没有和该控制端配对".to_string();
                        reject(channel, &e).await;
                        return Err(e);
                    }
                },
            };
            let responder = Responder::new(psk, &hello);
            channel
                .send(&Wire::Frame(Frame::HelloOk(responder.reply())))
                .await?;
            match channel.recv().await? {
                Wire::Frame(Frame::Confirm(confirm)) => match responder.complete(&confirm) {
                    Ok(cipher) => channel.encrypt(cipher),
                    Err(e) => {
                        reject(channel, &e).await;
                        return Err(e);
                    }
                },
                other => {
                    let e = format!("握手应答不对：{other:?}");
                    reject(channel, &e).await;
                    return Err(e);
                }
            }
            (hello.hub_id, pairing)
        }
        other => {
            let e = format!("应当先握手：{other:?}");
            reject(channel, &e).await;
            return Err(e);
        }
    };
    // 首次配对：第一件事实成长期密钥
    if pairing {
        match channel.recv().await? {
            Wire::Frame(Frame::Rotate { psk }) => {
                let psk: Psk = URL_SAFE_NO_PAD
                    .decode(&psk)
                    .map_err(|_| "新密钥编码不合法".to_string())?
                    .try_into()
                    .map_err(|_| "新密钥长度不对".to_string())?;
                auth.paired(&hub_id, psk);
                channel.send(&Wire::Frame(Frame::Rotated {})).await?;
            }
            other => {
                let e = format!("配对应先换成长期密钥：{other:?}");
                reject(channel, &e).await;
                return Err(e);
            }
        }
    }

    // ---- 请求 / 应答 ----
    let (out_tx, mut out_rx) = mpsc::channel::<Wire>(QUEUE);

    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_rx = Instant::now();
    let conn = CONNECTION.fetch_add(1, Ordering::SeqCst);
    loop {
        tokio::select! {
            wire = out_rx.recv() => match wire {
                Some(wire) => channel.send(&wire).await?,
                None => break,
            },
            incoming = channel.recv() => {
                let wire = incoming?;
                last_rx = Instant::now();
                match wire {
                    Wire::Frame(Frame::Request { id, method, params }) => {
                        handle(
                            id,
                            method,
                            params,
                            conn,
                            pairing,
                            Arc::clone(&auth),
                            Arc::clone(&service),
                            out_tx.clone(),
                            Arc::clone(&turns),
                        );
                    }
                    Wire::Frame(Frame::Ping { nonce }) => {
                        channel.send(&Wire::Frame(Frame::Pong { nonce })).await?;
                    }
                    _ => {}
                }
            }
            _ = ticker.tick() => {
                if last_rx.elapsed() > NODE_IDLE {
                    return Err(format!("控制端 {} 秒没有任何消息，断开连接", last_rx.elapsed().as_secs()));
                }
            }
        }
    }
    // 走到这里说明写出队列关了（本地任务都已结束）
    #[allow(unreachable_code)]
    Err("连接已断开".into())
}

/// 处理一个请求。耗时的请求放到独立任务里，不挡住后续请求与保活。
#[allow(clippy::too_many_arguments)]
fn handle(
    id: u64,
    method: String,
    params: Value,
    conn: u64,
    // 这次连接是不是首次配对（靠配对码连上来的）
    pairing: bool,
    auth: Arc<dyn NodeAuth>,
    service: Arc<Service>,
    out: mpsc::Sender<Wire>,
    turns: TurnSet,
) {
    tokio::spawn(async move {
        // 靠配对码连上来的这一次连接不能为别的设备开放配对
        let result = if method == "pair.allow" {
            if pairing {
                Err("只有已配对的控制端才能开放配对".into())
            } else {
                match params["code"].as_str().filter(|c| c.len() >= 8) {
                    Some(code) => {
                        auth.allow_pairing(code);
                        Ok(json!({}))
                    }
                    None => Err("配对码不合法".into()),
                }
            }
        } else {
            let mut params = params;
            // 对话标识带上连接序号：不同控制端的会话号可能相同，不能撞在一起
            let turn_key = match &mut params {
                Value::Object(map) if method == "turn.start" || method == "turn.stop" => {
                    let key = format!(
                        "{conn}:{}",
                        map.get("turnId").and_then(Value::as_str).unwrap_or_default()
                    );
                    map.insert("turnId".into(), Value::String(key.clone()));
                    key
                }
                _ => String::new(),
            };
            let (tx, rx) = mpsc::channel::<TurnEvent>(QUEUE);
            if method == "turn.start" {
                turns.lock().unwrap().insert(turn_key.clone());
                let (out, service) = (out.clone(), Arc::clone(&service));
                let finished = Arc::clone(&turns);
                let key = turn_key.clone();
                tokio::spawn(async move {
                    forward(rx, out, conn, key.clone(), service).await;
                    finished.lock().unwrap().remove(&key);
                });
            }
            service.call(&method, &params, tx).await
        };
        let frame = match result {
            Ok(result) => Frame::ok(id, result),
            Err(error) => Frame::err(id, error),
        };
        let _ = out.send(Wire::Frame(frame)).await;
    });
}

/// 把一轮对话的事件推回控制端：去掉连接前缀；连接断开就停掉这一轮。
async fn forward(
    mut rx: mpsc::Receiver<TurnEvent>,
    out: mpsc::Sender<Wire>,
    conn: u64,
    turn_key: String,
    service: Arc<Service>,
) {
    let prefix = format!("{conn}:");
    while let Some(event) = rx.recv().await {
        let event = event.map_turn_id(|id| {
            id.strip_prefix(&prefix).unwrap_or(id.as_str()).to_string()
        });
        if out.send(Wire::Event(event)).await.is_err() {
            break;
        }
    }
    service.stop_turn(&turn_key);
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::runner::BinaryResolver;
    use crate::service::ServiceOptions;

    /// 造一个冒充 claude 的脚本，并返回只认它的服务
    fn fake_agent(script: &str) -> Arc<Service> {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        std::fs::write(&bin, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        std::mem::forget(dir);
        Service::new(ServiceOptions {
            name: Some("test".into()),
            resolve: Some(Arc::new(move |_: &str| Some(bin.clone())) as BinaryResolver),
            ..Default::default()
        })
    }

    fn auth(pairs: &[(&str, Psk)]) -> Arc<NodeState> {
        NodeState::new(
            "node-1".into(),
            pairs.iter().map(|(id, psk)| (id.to_string(), *psk)).collect(),
            None,
        )
    }

    /// 在一条内存连接上握手，返回控制端这一头
    async fn connect(
        service: Arc<Service>,
        auth: Arc<NodeState>,
        psk: Psk,
        rotate: Option<Psk>,
    ) -> Result<HubLink, String> {
        let (client_io, server_io) = tokio::io::duplex(1 << 20);
        tokio::spawn(async move {
            let ws = tokio_tungstenite::accept_async(server_io).await.unwrap();
            let _ = serve(Channel::new(ws), auth, service).await;
        });
        let (ws, _) = tokio_tungstenite::client_async("ws://node/", client_io)
            .await
            .unwrap();
        HubLink::handshake(Channel::new(ws), "hub-1", "node-1", psk, rotate).await
    }

    /// 取下一条事件（跳过被广播裁掉的提示）
    async fn next_event(rx: &mut broadcast::Receiver<TurnEvent>) -> TurnEvent {
        loop {
            match rx.recv().await {
                Ok(event) => return event,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => panic!("事件通道关闭了"),
            }
        }
    }

    async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !cond() {
            assert!(Instant::now() < deadline, "等待超时：{what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    const OK: &str = r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"s1"}' '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"远程"}}}' '{"type":"result","is_error":false,"result":"远程回答","total_cost_usd":0.05}'"#;

    /// 请求 / 应答，以及一轮远程对话的完整事件流
    #[tokio::test]
    async fn request_response_and_turn_events() {
        let service = fake_agent(OK);
        let psk = secure::random_psk();
        let link = connect(Arc::clone(&service), auth(&[("hub-1", psk)]), psk, None)
            .await
            .unwrap();

        let info = link.call("sys.info", json!({})).await.unwrap();
        assert_eq!(info["name"], "test");
        let ls = link
            .call("fs.ls", json!({ "path": std::env::temp_dir() }))
            .await
            .unwrap();
        assert!(ls["entries"].is_array());
        // 节点侧的错误原样带回
        assert!(link.call("nope", json!({})).await.unwrap_err().contains("不支持请求"));

        let mut events = link.subscribe();
        link.call(
            "turn.start",
            json!({ "turnId": "5", "adapter": "claude-cli", "prompt": "hi" }),
        )
        .await
        .unwrap();
        assert!(matches!(next_event(&mut events).await, TurnEvent::Session { ref r#ref, .. } if r#ref == "s1"));
        assert_eq!(
            next_event(&mut events).await,
            TurnEvent::Delta { turn_id: "5".into(), text: "远程".into() }
        );
        let TurnEvent::Finished { turn_id, text, cost_usd, .. } = next_event(&mut events).await else {
            panic!("应当以 Finished 收尾");
        };
        assert_eq!((turn_id.as_str(), text.as_str()), ("5", "远程回答"));
        assert_eq!(cost_usd, Some(0.05));
        // 保活：往返耗时有了读数
        let rtt = link.ping().await.unwrap();
        assert!(link.rtt_ms().is_some() && rtt < Duration::from_secs(5));
        // 主动断开：收发循环结束，等待方拿到原因
        let waiter = link.clone();
        link.close();
        assert_eq!(waiter.wait_closed().await, "控制端主动断开");
        assert!(link.closed_reason().is_some());
    }

    /// 密钥不对：握手被节点拒绝，拿不到可用连接
    #[tokio::test]
    async fn wrong_psk_is_rejected() {
        let service = fake_agent(OK);
        // 两边都会发现密钥不一致：控制端验不过节点的应答
        let err = connect(service, auth(&[("hub-1", secure::random_psk())]), secure::random_psk(), None)
            .await
            .unwrap_err();
        assert!(err.contains("预共享密钥不匹配"), "{err}");
    }

    /// 没配对过、也没有配对码：拒绝
    #[tokio::test]
    async fn unpaired_hub_without_a_code_is_rejected() {
        let service = fake_agent(OK);
        let auth = auth(&[]);
        let err = connect(service, Arc::clone(&auth), secure::random_psk(), None)
            .await
            .unwrap_err();
        assert!(err.contains("没有和该控制端配对"), "{err}");
        assert_eq!(auth.paired_count(), 0);
    }

    /// 首次配对：用配对码握手 → 换成长期密钥 → 以后用它连接；配对码只能用一次
    #[tokio::test]
    async fn pairing_rotates_to_a_long_term_key() {
        let service = fake_agent(OK);
        let node = auth(&[]);
        node.allow_pairing("K7QM-2X9P-4R8T-6WYZ");
        assert!(node.is_pairing_open());

        // 用配对码握手却不换密钥：节点不处理任何请求，直接断开
        let sloppy = connect(Arc::clone(&service), Arc::clone(&node), secure::psk_from_code("K7QM-2X9P-4R8T-6WYZ"), None)
            .await
            .unwrap();
        let err = sloppy.call("sys.info", json!({})).await.unwrap_err();
        assert!(err.contains("先换成长期密钥"), "{err}");
        // 配对码没被消耗
        assert!(node.is_pairing_open());

        let long_term = secure::random_psk();
        let link = connect(
            Arc::clone(&service),
            Arc::clone(&node),
            secure::psk_from_code("k7qm-2x9p-4r8t-6wyz"),
            Some(long_term),
        )
        .await
        .unwrap();
        assert_eq!(node.paired_count(), 1);
        assert!(!node.is_pairing_open(), "配对码只能用一次");
        assert_eq!(link.call("sys.info", json!({})).await.unwrap()["name"], "test");

        // 之后用长期密钥连接
        let again = connect(Arc::clone(&service), Arc::clone(&node), long_term, None)
            .await
            .unwrap();
        assert_eq!(again.call("sys.info", json!({})).await.unwrap()["name"], "test");
        // 配对码已经不能再用
        assert!(connect(Arc::clone(&service), Arc::clone(&node), secure::psk_from_code("k7qm-2x9p-4r8t-6wyz"), Some(secure::random_psk()))
            .await
            .is_err());
    }

    /// 配对码过期后失效
    #[tokio::test(start_paused = true)]
    async fn pairing_code_expires() {
        let node = auth(&[]);
        node.allow_pairing("K7QM-2X9P-4R8T-6WYZ");
        assert!(node.is_pairing_open());
        tokio::time::sleep(PAIRING_TTL + Duration::from_secs(1)).await;
        assert!(!node.is_pairing_open());
        assert!(NodeAuth::pending_psk(&*node).is_none());
    }

    /// 不同连接上的会话号可能相同：节点给它们加前缀，互不干扰
    #[tokio::test]
    async fn turn_ids_are_namespaced_per_connection() {
        let service = fake_agent("sleep 30");
        let psk = secure::random_psk();
        let node = auth(&[("hub-1", psk)]);
        let a = connect(Arc::clone(&service), Arc::clone(&node), psk, None).await.unwrap();
        let b = connect(Arc::clone(&service), Arc::clone(&node), psk, None).await.unwrap();
        for link in [&a, &b] {
            link.call("turn.start", json!({ "turnId": "1", "adapter": "claude-cli", "prompt": "hi" }))
                .await
                .unwrap();
        }
        assert_eq!(service.running_turns(), 2, "两条连接上同名会话应当都能启动");
        let mut a_events = a.subscribe();
        let mut b_events = b.subscribe();
        // 停 b 上的那一轮，不影响 a 上的
        b.call("turn.stop", json!({ "turnId": "1" })).await.unwrap();
        wait_until("b 上的对话被停掉", || service.running_turns() == 1).await;
        // b 那一轮以「已停止」收尾，且事件还是原样的会话号
        let event = next_event(&mut b_events).await;
        assert!(
            matches!(&event, TurnEvent::Failed { turn_id, .. } if turn_id == "1"),
            "{event:?}"
        );
        a.call("turn.stop", json!({ "turnId": "1" })).await.unwrap();
        assert!(matches!(next_event(&mut a_events).await, TurnEvent::Failed { .. }));
        wait_until("全部停掉", || service.running_turns() == 0).await;
    }

    /// 控制端断开后，它在这台机器上启动的对话随之停止（哪怕这一轮正安静地等着）
    #[tokio::test]
    async fn dropping_the_link_stops_its_turns() {
        let service = fake_agent("sleep 30");
        let psk = secure::random_psk();
        let link = connect(Arc::clone(&service), auth(&[("hub-1", psk)]), psk, None)
            .await
            .unwrap();
        link.call("turn.start", json!({ "turnId": "9", "adapter": "claude-cli", "prompt": "hi" }))
            .await
            .unwrap();
        assert_eq!(service.running_turns(), 1);
        drop(link);
        wait_until("对话随连接一起停掉", || service.running_turns() == 0).await;
        // 连接断了之后发起的请求会立刻失败，而不是一直等
        let psk2 = secure::random_psk();
        let link = connect(Arc::clone(&service), auth(&[("hub-1", psk2)]), psk2, None)
            .await
            .unwrap();
        link.close();
        let err = tokio::time::timeout(Duration::from_secs(5), link.call("sys.info", json!({})))
            .await
            .expect("不应一直等")
            .unwrap_err();
        assert!(!err.is_empty());
    }

    /// 只有已配对的控制端才能为别的设备开放配对
    #[tokio::test]
    async fn pair_allow_requires_a_paired_hub() {
        let service = fake_agent(OK);
        let psk = secure::random_psk();
        let node = auth(&[("hub-1", psk)]);
        let link = connect(Arc::clone(&service), Arc::clone(&node), psk, None)
            .await
            .unwrap();
        assert!(link
            .call("pair.allow", json!({ "code": "short" }))
            .await
            .unwrap_err()
            .contains("不合法"));
        // 靠配对码连上来的连接不能开放配对：换一个只有配对码的节点状态验证
        link.call("pair.allow", json!({ "code": "ABCD-EFGH-JKLM-NPQR" }))
            .await
            .unwrap();
        assert!(node.is_pairing_open());
        // 第二台设备凭这个配对码接入
        let hub2 = secure::random_psk();
        let (client_io, server_io) = tokio::io::duplex(1 << 20);
        let (service2, node2) = (Arc::clone(&service), Arc::clone(&node));
        tokio::spawn(async move {
            let ws = tokio_tungstenite::accept_async(server_io).await.unwrap();
            let _ = serve(Channel::new(ws), node2, service2).await;
        });
        let (ws, _) = tokio_tungstenite::client_async("ws://node/", client_io).await.unwrap();
        HubLink::handshake(
            Channel::new(ws),
            "hub-2",
            "node-1",
            secure::psk_from_code("ABCD-EFGH-JKLM-NPQR"),
            Some(hub2),
        )
        .await
        .unwrap();
        assert_eq!(node.paired_count(), 2);
    }

    /// 节点只认自己的 id：控制端找错节点时直接拒绝
    #[tokio::test]
    async fn wrong_node_id_is_rejected() {
        let service = fake_agent(OK);
        let psk = secure::random_psk();
        let (client_io, server_io) = tokio::io::duplex(1 << 20);
        let (service, node) = (service, auth(&[("hub-1", psk)]));
        tokio::spawn(async move {
            let ws = tokio_tungstenite::accept_async(server_io).await.unwrap();
            let _ = serve(Channel::new(ws), node, service).await;
        });
        let (ws, _) = tokio_tungstenite::client_async("ws://node/", client_io).await.unwrap();
        let err = HubLink::handshake(Channel::new(ws), "hub-1", "node-2", psk, None)
            .await
            .unwrap_err();
        assert!(err.contains("找错节点"), "{err}");
    }
}
