//! 远程节点：配对、连接、状态与请求转发。
//!
//! 每台配对过的机器有一个常驻的连接任务（见 [`Supervisor::run`]）：连上后保持端到端
//! 加密的连接、转发对话事件、感知断线；断了按退避重连，界面通过 `node-event` 得知。
//!
//! 接入有两条路，都只需要被管理的机器能出站访问：
//! - 经中继（推荐）：节点与控制端各自连到自建中继，在 NAT 后面也能互通
//! - 直连：被管理的机器有公网地址时，控制端直接连它

use autoagents_core::discovery::DiscoveredAgent;
use autoagents_core::link::HubLink;
use autoagents_core::machine::MachineInfo;
use autoagents_core::net::{self, Fingerprint};
use autoagents_core::relay::{self, NodeSummary, RelayWatcher};
use autoagents_core::secure::{self, Psk};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::{broadcast, mpsc, watch};

use crate::events;
use crate::secrets::{self, SecretStore};
use crate::AppState;

const SETTINGS_RELAY_ADDR: &str = "relay_addr";
const SETTINGS_RELAY_FP: &str = "relay_fingerprint";
const SETTINGS_INSTALL_SCRIPT: &str = "install_script";
const SETTINGS_INVITES: &str = "pair_invites";
/// agentd 安装脚本的默认地址（仓库 main 分支）
const DEFAULT_INSTALL_SCRIPT: &str =
    "https://raw.githubusercontent.com/1989tianlong/AutoAgents/main/scripts/install-agentd.sh";
const ACCOUNT_RELAY_TOKEN: &str = "relay_token";
const ACCOUNT_HUB_ID: &str = "hub_id";
/// 重连退避范围
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// 配对码有效期（与 agentd 一致）
const PAIRING_TTL_S: u64 = 600;

// ---------------------------------------------------------------------------
// 给界面看的类型
// ---------------------------------------------------------------------------

/// 一台远程机器的接入配置
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeConfig {
    pub name: String,
    /// 非空表示经这个中继接入
    pub relay_url: String,
    /// 直连地址 `wss://主机:端口`
    pub addr: String,
    /// 直连时钉住的证书指纹；经中继时用中继的
    pub fingerprint: String,
}

/// 一台机器此刻的连接状态
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeStatus {
    pub online: bool,
    /// 最近一次连不上或断开的原因
    pub error: Option<String>,
    /// 这次连上的时刻（Unix 毫秒）
    pub since_ms: Option<u64>,
    /// 最近一次在线的时刻
    pub last_seen_ms: Option<u64>,
    /// 保活往返耗时
    pub rtt_ms: Option<u64>,
}

/// 给界面看的一台机器
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeView {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub status: NodeStatus,
    pub relay_url: String,
    pub addr: String,
    pub info: Option<MachineInfo>,
    pub agents: Vec<DiscoveredAgent>,
}

/// 推给界面的机器状态变化
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeStatusEvent {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub message: Option<String>,
}

/// 把机器状态变化送到界面
pub trait NodeSink: Send + Sync + 'static {
    fn send(&self, event: NodeStatusEvent);
}

pub struct TauriNodeSink(pub AppHandle);

impl NodeSink for TauriNodeSink {
    fn send(&self, event: NodeStatusEvent) {
        if let Err(e) = self.0.emit("node-event", event) {
            eprintln!("[remote] 推送机器状态失败：{e}");
        }
    }
}

/// 中继配置
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayView {
    pub configured: bool,
    pub addr: String,
    pub fingerprint: String,
    pub has_token: bool,
    /// 给新机器用的安装脚本地址
    pub install_script: String,
}

/// 中继上一个在线的节点
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OnlineNode {
    #[serde(flatten)]
    pub summary: NodeSummary,
    /// 是否已经和本机配对
    pub paired: bool,
}

/// 一份还没用掉的接入邀请：安装命令已经发给某台机器，等它连上来
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteView {
    pub name: String,
    /// 剩余有效期（秒）
    pub expires_in_s: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayStatusView {
    pub relay: RelayView,
    pub connected: bool,
    pub message: Option<String>,
    pub online: Vec<OnlineNode>,
    /// 等着接入的机器
    pub pending: Vec<InviteView>,
}

/// 给新机器用的安装命令
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallCommand {
    pub name: String,
    pub code: String,
    pub command: String,
    /// 配对码有效期（秒）
    pub expires_in_s: u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 设置、节点表、Keychain 的读写
// ---------------------------------------------------------------------------

pub fn setting(db: &Connection, key: &str) -> Option<String> {
    db.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        params![key],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
    .filter(|value| !value.is_empty())
}

pub fn set_setting(db: &Connection, key: &str, value: &str) -> Result<(), String> {
    db.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

const NODE_COLS: &str = "id, name, relay_url, addr, fingerprint, info";

fn row_to_node(row: &rusqlite::Row) -> rusqlite::Result<(String, NodeConfig, Option<MachineInfo>)> {
    let id: String = row.get(0)?;
    let config = NodeConfig {
        name: row.get(1)?,
        relay_url: row.get(2)?,
        addr: row.get(3)?,
        fingerprint: row.get(4)?,
    };
    let info: Option<MachineInfo> = row
        .get::<_, Option<String>>(5)?
        .and_then(|text| serde_json::from_str(&text).ok());
    Ok((id, config, info))
}

fn load_nodes(db: &Connection) -> Result<Vec<(String, NodeConfig, Option<MachineInfo>)>, String> {
    let mut stmt = db
        .prepare_cached(&format!(
            "SELECT {NODE_COLS} FROM nodes ORDER BY created_at, id"
        ))
        .map_err(|e| e.to_string())?;
    stmt.query_map([], row_to_node)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

fn upsert_node(
    db: &Connection,
    id: &str,
    config: &NodeConfig,
    info: Option<&MachineInfo>,
) -> Result<(), String> {
    let info = info.map(serde_json::to_string).transpose().map_err(|e| e.to_string())?;
    db.execute(
        "INSERT INTO nodes (id, name, relay_url, addr, fingerprint, info) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            relay_url = excluded.relay_url,
            addr = excluded.addr,
            fingerprint = excluded.fingerprint,
            info = CASE WHEN excluded.info IS NULL THEN nodes.info ELSE excluded.info END",
        params![id, config.name, config.relay_url, config.addr, config.fingerprint, info],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 从一段输入里解析中继的接入参数：接受连接串
/// `autoagents://relay?addr=…&token=…&fp=…`，也接受裸地址 `wss://主机:端口`
#[derive(Debug, Default, PartialEq)]
pub struct ParsedRelay {
    pub addr: Option<String>,
    pub token: Option<String>,
    pub fingerprint: Option<String>,
}

pub fn parse_relay_input(text: &str) -> ParsedRelay {
    let text = text.trim();
    if text.is_empty() {
        return ParsedRelay::default();
    }
    let Some((scheme, query)) = text.split_once('?') else {
        return ParsedRelay {
            addr: Some(text.to_string()),
            ..Default::default()
        };
    };
    if !scheme.starts_with("autoagents://") {
        return ParsedRelay {
            addr: Some(text.to_string()),
            ..Default::default()
        };
    }
    let mut parsed = ParsedRelay::default();
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = percent_decode(value);
        match key {
            "addr" => parsed.addr = Some(value),
            "token" => parsed.token = Some(value),
            "fp" | "fingerprint" => parsed.fingerprint = Some(value),
            _ => {}
        }
    }
    parsed
}

/// 节点的直连连接串（`agentd pair` 打印的那一行）：
/// `autoagents://node?addr=wss://…&fp=…&id=…&name=…&pair=…`
pub fn parse_node_input(text: &str) -> Result<(String, Option<Fingerprint>, String, Option<String>, String), String> {
    let text = text.trim();
    let missing = |what: &str| format!("连接串里缺少{what}，请把 agentd pair 打印的那一行原样粘贴进来");
    let query = text
        .strip_prefix("autoagents://node?")
        .ok_or_else(|| "不是节点的连接串（应以 autoagents://node? 开头）".to_string())?;
    let mut fields = HashMap::new();
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        fields.insert(key.to_string(), percent_decode(value));
    }
    let field = |key: &str| fields.get(key).cloned().filter(|v| !v.is_empty());
    let addr = field("addr").ok_or_else(|| missing("地址 addr"))?;
    let id = field("id").ok_or_else(|| missing("节点 id"))?;
    let code = field("pair").ok_or_else(|| missing("配对码 pair"))?;
    let fingerprint = match field("fp") {
        Some(text) => Some(Fingerprint::parse(&text)?),
        None => None,
    };
    Ok((addr, fingerprint, id, field("name"), code))
}

/// 连接串里的百分号编码（名字里的空格、中文等）
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            out.push(u8::from_str_radix(&text[i + 1..i + 3], 16).expect("已确认是十六进制"));
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 放进 shell 命令时给参数加引号
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// 本应用在各个节点那里的身份；第一次使用时生成
fn hub_id(secrets: &dyn SecretStore) -> Result<String, String> {
    if let Some(id) = secrets.get(ACCOUNT_HUB_ID)? {
        return Ok(id);
    }
    let id = URL_SAFE_NO_PAD.encode(secure::random::<12>());
    secrets.set(ACCOUNT_HUB_ID, &id)?;
    Ok(id)
}

fn relay_view(db: &Connection, secrets: &dyn SecretStore) -> Result<RelayView, String> {
    let addr = setting(db, SETTINGS_RELAY_ADDR).unwrap_or_default();
    let has_token = secrets.get(ACCOUNT_RELAY_TOKEN)?.is_some();
    Ok(RelayView {
        configured: !addr.is_empty() && has_token,
        addr,
        fingerprint: setting(db, SETTINGS_RELAY_FP).unwrap_or_default(),
        has_token,
        install_script: setting(db, SETTINGS_INSTALL_SCRIPT)
            .unwrap_or_else(|| DEFAULT_INSTALL_SCRIPT.to_string()),
    })
}

type RelayParams = (String, Option<Fingerprint>, String);

fn secrets_get(store: &dyn SecretStore, account: &str) -> Result<Option<String>, String> {
    store.get(account)
}

/// 中继的接入参数；没配置时给出明确提示
fn relay_params(db: &Connection, secrets: &dyn SecretStore) -> Result<RelayParams, String> {
    let addr = setting(db, SETTINGS_RELAY_ADDR)
        .ok_or("还没有设置中继：到「机器」页粘贴中继的连接串")?;
    let fingerprint = setting(db, SETTINGS_RELAY_FP).unwrap_or_default();
    // 没填指纹时按正常 CA 校验（中继放在正规证书的反向代理后面）
    let pin = if fingerprint.is_empty() {
        None
    } else {
        Some(Fingerprint::parse(&fingerprint)?)
    };
    let token = secrets
        .get(ACCOUNT_RELAY_TOKEN)?
        .ok_or("中继令牌丢了：请重新粘贴中继的连接串")?;
    Ok((addr, pin, token))
}

fn decode_psk(text: &str) -> Option<Psk> {
    URL_SAFE_NO_PAD
        .decode(text)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
}

/// 一份接入邀请：安装命令里的配对码，等那台机器连上来配对
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Invite {
    name: String,
    code: String,
    created_ms: u64,
}

impl Invite {
    fn new(name: &str, code: String) -> Self {
        Self {
            name: name.to_string(),
            code,
            created_ms: now_ms(),
        }
    }

    /// 有效期剩余（秒）
    fn expires_in_s(&self) -> u64 {
        PAIRING_TTL_S.saturating_sub(now_ms().saturating_sub(self.created_ms) / 1000)
    }

    fn expired(&self) -> bool {
        self.expires_in_s() == 0
    }
}

fn load_invites(db: &Connection) -> Vec<Invite> {
    setting(db, SETTINGS_INVITES)
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_invites(db: &Connection, invites: &[Invite]) -> Result<(), String> {
    let text = serde_json::to_string(invites).map_err(|e| e.to_string())?;
    set_setting(db, SETTINGS_INVITES, &text)
}

// ---------------------------------------------------------------------------
// 一台远程机器的运行时状态
// ---------------------------------------------------------------------------

struct Remote {
    id: String,
    cfg: Mutex<NodeConfig>,
    info: Mutex<Option<MachineInfo>>,
    agents: Mutex<Vec<DiscoveredAgent>>,
    link: Mutex<Option<HubLink>>,
    status: Mutex<NodeStatus>,
    shutdown: watch::Sender<bool>,
    /// 让连接任务跳过退避、立刻重连
    kick: watch::Sender<u64>,
}

impl Remote {
    fn new(id: String, cfg: NodeConfig, info: Option<MachineInfo>) -> Arc<Self> {
        Arc::new(Self {
            id,
            cfg: Mutex::new(cfg),
            info: Mutex::new(info),
            agents: Mutex::new(Vec::new()),
            link: Mutex::new(None),
            status: Mutex::new(NodeStatus::default()),
            shutdown: watch::Sender::new(false),
            kick: watch::Sender::new(0),
        })
    }

    fn name(&self) -> String {
        self.cfg.lock().unwrap().name.clone()
    }

    fn set_online(&self, rtt_ms: Option<u64>) {
        let mut status = self.status.lock().unwrap();
        status.online = true;
        status.error = None;
        status.since_ms = Some(now_ms());
        status.last_seen_ms = Some(now_ms());
        status.rtt_ms = rtt_ms;
    }

    /// 记为离线，返回状态是否变了（在线 ↔ 离线，或原因不同）。
    /// 没变就不必再通知界面：按退避反复重连时，同样的原因会一遍遍出现。
    fn set_offline(&self, error: Option<String>) -> bool {
        let mut status = self.status.lock().unwrap();
        let changed = status.online || status.error != error;
        if status.online {
            status.last_seen_ms = Some(now_ms());
        }
        status.online = false;
        status.error = error;
        status.since_ms = None;
        status.rtt_ms = None;
        changed
    }

    fn view(&self) -> NodeView {
        let cfg = self.cfg.lock().unwrap().clone();
        let status = self.status.lock().unwrap().clone();
        NodeView {
            id: self.id.clone(),
            name: cfg.name.clone(),
            online: status.online,
            status,
            relay_url: cfg.relay_url,
            addr: cfg.addr,
            info: self.info.lock().unwrap().clone(),
            agents: self.agents.lock().unwrap().clone(),
        }
    }

    fn event(&self, message: Option<String>) -> NodeStatusEvent {
        NodeStatusEvent {
            id: self.id.clone(),
            name: self.name(),
            online: self.status.lock().unwrap().online,
            message,
        }
    }
}

type OfflineHook = Arc<dyn Fn(&str) + Send + Sync>;

/// 一条连接守到头的几种结局
enum Outcome {
    /// 连上过，后来断了
    Dropped,
    /// 没连上
    Failed,
    /// 被要求重连（换了中继配置，或用户点了「立即重连」）
    Kicked,
    /// 机器被移除
    Removed,
}

/// 连接任务需要的东西。各留一份，任务里不再借用管理器。
#[derive(Clone)]
struct Supervisor {
    db: Arc<Mutex<Connection>>,
    secrets: Arc<dyn SecretStore>,
    events: mpsc::UnboundedSender<crate::chat::NodeEvent>,
    node_sink: Arc<dyn NodeSink>,
    on_offline: Arc<Mutex<Option<OfflineHook>>>,
}

impl Supervisor {
    fn record(&self, kind: &str, level: &str, source: &str, message: &str) {
        let db = self.db.lock().unwrap();
        events::record(&db, kind, level, source, message, Some("#/machines"));
    }

    /// 这台机器上正在运行的对话不可能再有结果了：让它们以失败收尾
    fn end_turns(&self, node_id: &str) {
        let hook = self.on_offline.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(node_id);
        }
    }

    /// 与这台机器配对时存下的长期密钥
    fn paired_key(&self, remote: &Remote) -> Result<Psk, String> {
        let stored = self
            .secrets
            .get(&secrets::node_psk_account(&remote.id))?
            .ok_or_else(|| "没有保存与这台机器的配对密钥：请移除后重新配对".to_string())?;
        decode_psk(&stored).ok_or_else(|| "配对密钥无法解析：请移除后重新配对".to_string())
    }

    /// 保持与一台机器的连接，直到它被移除。
    async fn run(self, remote: Arc<Remote>, mut current: Option<HubLink>) {
        let mut backoff = BACKOFF_MIN;
        let mut forced = false;
        loop {
            if *remote.shutdown.borrow() {
                return;
            }
            let link = match current.take() {
                // 刚配对成功：这条连接直接拿来用
                Some(link) => Ok(link),
                None => {
                    let attempt = async {
                        let hub_id = hub_id(self.secrets.as_ref())?;
                        let psk = self.paired_key(&remote)?;
                        connect(self.db.clone(), self.secrets.clone(), &remote, psk, &hub_id)
                            .await
                    };
                    attempt.await
                }
            };
            let outcome = match link {
                Ok(link) => self.hold(Arc::clone(&remote), link).await,
                Err(e) => {
                    // 原因没变就不重复通知；但用户点了「立即重连」的那一次总要有个回音
                    if remote.set_offline(Some(e.clone())) || forced {
                        self.node_sink.send(remote.event(Some(e)));
                    }
                    Outcome::Failed
                }
            };
            if matches!(outcome, Outcome::Removed) {
                return;
            }
            // 等一会儿再连。「立刻重连」不用等；「移除」直接收工
            let delay = match outcome {
                Outcome::Kicked => Duration::ZERO,
                // 刚才还连着，说明配置没问题：从头退避
                Outcome::Dropped => {
                    backoff = BACKOFF_MIN;
                    BACKOFF_MIN
                }
                _ => backoff,
            };
            forced = matches!(outcome, Outcome::Kicked);
            let mut stop = remote.shutdown.subscribe();
            let mut kick = remote.kick.subscribe();
            kick.borrow_and_update();
            if !delay.is_zero() {
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = kick.changed() => forced = true,
                    _ = stop.changed() => return,
                }
            }
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
    }

    /// 守着一条连上的连接：转发对话事件、保持在线，直到它断开、被要求重连或机器被移除。
    async fn hold(&self, remote: Arc<Remote>, link: HubLink) -> Outcome {
        // 连上的过程中机器被移除了：收工（新订阅的接收方要等下一次变化，这里得先看一眼当前值）
        if *remote.shutdown.borrow() {
            link.close();
            return Outcome::Removed;
        }
        let mut stop = remote.shutdown.subscribe();
        let mut kick = remote.kick.subscribe();
        kick.borrow_and_update();
        *remote.link.lock().unwrap() = Some(link.clone());
        self.on_online(&remote, &link).await;
        self.node_sink.send(remote.event(None));

        // 把这台机器推来的对话事件汇入事件总线（按会话归档由 chat 模块负责）
        let forward = {
            let (bus, id) = (self.events.clone(), remote.id.clone());
            let mut rx = link.subscribe();
            tauri::async_runtime::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(event) => {
                            if bus
                                .send(crate::chat::NodeEvent {
                                    node: id.clone(),
                                    event,
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            })
        };
        enum End {
            Closed(String),
            Kicked,
            Removed,
        }
        let end = tokio::select! {
            reason = link.wait_closed() => End::Closed(reason),
            _ = kick.changed() => End::Kicked,
            _ = stop.changed() => End::Removed,
        };
        // 主动重连、移除时立刻断开：节点看到连接断开就会停掉这条连接上的对话
        link.close();
        forward.abort();
        remote.link.lock().unwrap().take();
        match end {
            End::Removed => Outcome::Removed,
            End::Kicked => {
                self.end_turns(&remote.id);
                Outcome::Kicked
            }
            End::Closed(reason) => {
                // 它上面正在运行的会话不可能再有结果了
                self.end_turns(&remote.id);
                if remote.set_offline(Some(reason.clone())) {
                    self.node_sink.send(remote.event(Some(reason.clone())));
                }
                self.record(
                    "node.offline",
                    "warning",
                    &remote.name(),
                    &format!("连接断开：{reason}"),
                );
                Outcome::Dropped
            }
        }
    }

    /// 连上之后：取机器信息与本机 Agent 清单，更新往返耗时
    async fn on_online(&self, remote: &Remote, link: &HubLink) {
        if let Ok(info) = link.call("sys.info", json!({})).await {
            if let Ok(info) = serde_json::from_value::<MachineInfo>(info) {
                let config = remote.cfg.lock().unwrap().clone();
                let db = self.db.lock().unwrap();
                let _ = upsert_node(&db, &remote.id, &config, Some(&info));
                drop(db);
                *remote.info.lock().unwrap() = Some(info);
            }
        }
        if let Ok(list) = link.call("agents.list", json!({})).await {
            *remote.agents.lock().unwrap() = serde_json::from_value(list).unwrap_or_default();
        }
        if let Ok(rtt) = link.ping().await {
            remote.status.lock().unwrap().rtt_ms = Some(rtt.as_millis() as u64);
        }
        remote.set_online(link.rtt_ms());
        let version = remote
            .info
            .lock()
            .unwrap()
            .as_ref()
            .map(|info| format!("，agentd v{}", info.version))
            .unwrap_or_default();
        self.record(
            "node.online",
            "info",
            &remote.name(),
            &format!("机器已连接{version}"),
        );
    }
}

/// 连到一台机器：经中继或直连，然后完成端到端加密握手
async fn connect(
    db: Arc<Mutex<Connection>>,
    secrets: Arc<dyn SecretStore>,
    remote: &Remote,
    psk: Psk,
    hub_id: &str,
) -> Result<HubLink, String> {
    let cfg = remote.cfg.lock().unwrap().clone();
    let name = remote.name();
    let channel = if cfg.relay_url.is_empty() {
        let pin = if cfg.fingerprint.is_empty() {
            None
        } else {
            Some(Fingerprint::parse(&cfg.fingerprint)?)
        };
        net::dial(&cfg.addr, pin.as_ref()).await?
    } else {
        let (addr, pin, token) = {
            let db = db.lock().unwrap();
            relay_params(&db, secrets.as_ref())?
        };
        relay::open(&addr, pin.as_ref(), &token, &remote.id)
            .await
            .map_err(|e| format!("经中继连到「{name}」失败：{e}"))?
    };
    HubLink::handshake(channel, hub_id, &remote.id, psk, None)
        .await
        .map_err(|e| format!("与「{name}」握手失败：{e}"))
}

// ---------------------------------------------------------------------------
// 远程节点管理
// ---------------------------------------------------------------------------

pub struct RemoteManager {
    db: Arc<Mutex<Connection>>,
    secrets: Arc<dyn SecretStore>,
    node_sink: Arc<dyn NodeSink>,
    nodes: Mutex<HashMap<String, Arc<Remote>>>,
    /// 正在配对中的机器（自动配对与手动配对可能同时撞上同一台）
    pairing: Mutex<HashSet<String>>,
    watcher: Mutex<Option<RelayWatcher>>,
    supervisor: Supervisor,
}

impl RemoteManager {
    pub fn new(
        db: Arc<Mutex<Connection>>,
        secrets: Arc<dyn SecretStore>,
        events: mpsc::UnboundedSender<crate::chat::NodeEvent>,
        node_sink: Arc<dyn NodeSink>,
    ) -> Self {
        Self {
            supervisor: Supervisor {
                db: Arc::clone(&db),
                secrets: Arc::clone(&secrets),
                events,
                node_sink: Arc::clone(&node_sink),
                on_offline: Arc::default(),
            },
            db,
            secrets,
            node_sink,
            nodes: Mutex::new(HashMap::new()),
            pairing: Mutex::new(HashSet::new()),
            watcher: Mutex::new(None),
        }
    }

    /// 节点掉线时调用（用来结束它上面正在运行的会话）
    pub fn set_on_offline(&self, callback: OfflineHook) {
        *self.supervisor.on_offline.lock().unwrap() = Some(callback);
    }

    fn relay_configured(&self) -> bool {
        relay_view(&self.db.lock().unwrap(), self.secrets.as_ref())
            .is_ok_and(|relay| relay.configured)
    }

    /// 应用启动：连上所有配对过的机器；配了中继就开始跟踪在线节点
    pub fn start_all(self: &Arc<Self>) {
        let nodes = {
            let db = self.db.lock().unwrap();
            load_nodes(&db).unwrap_or_default()
        };
        for (id, config, info) in nodes {
            let remote = Remote::new(id, config, info);
            self.nodes
                .lock()
                .unwrap()
                .insert(remote.id.clone(), Arc::clone(&remote));
            tauri::async_runtime::spawn(self.supervisor.clone().run(remote, None));
        }
        if self.relay_configured() {
            if let Err(e) = self.start_watcher() {
                eprintln!("[remote] 跟踪中继在线节点失败：{e}");
            }
        }
    }

    /// 按当前配置与中继保持一条控制连接（旧的先停掉），并开始盯着有没有机器等着自动配对。
    /// 接入参数不对时立刻报错；换连接的动作放进异步运行时里做（同步命令跑在主线程上，
    /// 那里没有 tokio 的运行时）。
    fn start_watcher(self: &Arc<Self>) -> Result<(), String> {
        let (addr, pin, token) = {
            let db = self.db.lock().unwrap();
            relay_params(&db, self.secrets.as_ref())?
        };
        let manager = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            let online = {
                let mut slot = manager.watcher.lock().unwrap();
                if let Some(old) = slot.take() {
                    old.stop();
                }
                let fresh = RelayWatcher::spawn(addr, pin, token);
                let online = fresh.online();
                *slot = Some(fresh);
                online
            };
            tauri::async_runtime::spawn(manager.auto_pair_loop(online));
        });
        Ok(())
    }

    /// 机器凭安装命令里的配对码连上中继之后自动完成配对，不必再手动点一下。
    /// 中继的在线名单一变就检查一遍，也定时检查（重试上次没配上的、清掉过期的邀请）。
    async fn auto_pair_loop(self: Arc<Self>, mut online: watch::Receiver<Vec<NodeSummary>>) {
        online.borrow_and_update();
        // 上次没配上的机器，过一会儿再试
        let mut retry_after: HashMap<String, Instant> = HashMap::new();
        let mut ticker = tokio::time::interval(Duration::from_secs(2));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                changed = online.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                _ = ticker.tick() => {}
            }
            if let Err(e) = self.drop_expired_invites() {
                eprintln!("[remote] 清理过期的接入邀请失败：{e}");
            }
            let (connected, summaries) = match self.watcher.lock().unwrap().as_ref() {
                Some(watcher) => {
                    let connected = watcher.status().connected;
                    (connected, watcher.online().borrow().clone())
                }
                None => return,
            };
            if !connected {
                continue;
            }
            let paired: HashSet<String> = self.nodes.lock().unwrap().keys().cloned().collect();
            let invites = load_invites(&self.db.lock().unwrap());
            for summary in summaries {
                if paired.contains(&summary.node_id) {
                    continue;
                }
                if retry_after
                    .get(&summary.node_id)
                    .is_some_and(|at| Instant::now() < *at)
                {
                    continue;
                }
                let Some(invite) = invites.iter().find(|invite| invite.name == summary.name) else {
                    continue;
                };
                match self
                    .pair(&summary.node_id, &invite.code, Some(&invite.name), None)
                    .await
                {
                    Ok(_) => {
                        retry_after.remove(&summary.node_id);
                        let _ = self.forget_invite(Some(&invite.code), None);
                    }
                    Err(e) => {
                        eprintln!(
                            "[remote] 与「{}」自动配对没有成功，稍后再试：{e}",
                            summary.name
                        );
                        retry_after.insert(
                            summary.node_id.clone(),
                            Instant::now() + Duration::from_secs(3),
                        );
                    }
                }
            }
        }
    }

    pub fn relay_status(&self) -> Result<RelayStatusView, String> {
        let relay = relay_view(&self.db.lock().unwrap(), self.secrets.as_ref())?;
        let (connected, message, online) = {
            let watcher = self.watcher.lock().unwrap();
            match watcher.as_ref() {
                Some(watcher) if watcher.status().connected => {
                    (true, None, watcher.online().borrow().clone())
                }
                // 断开时手里的名单是旧的，不能当作在线
                Some(watcher) => (false, watcher.status().message, Vec::new()),
                None => (
                    false,
                    relay.configured.then(|| "正在连接中继…".to_string()),
                    Vec::new(),
                ),
            }
        };
        let paired: HashSet<String> = self.nodes.lock().unwrap().keys().cloned().collect();
        let pending = load_invites(&self.db.lock().unwrap())
            .into_iter()
            .filter(|invite| !invite.expired())
            .map(|invite| InviteView {
                expires_in_s: invite.expires_in_s(),
                name: invite.name,
            })
            .collect();
        Ok(RelayStatusView {
            relay,
            connected,
            message,
            online: online
                .into_iter()
                .map(|summary| OnlineNode {
                    paired: paired.contains(&summary.node_id),
                    summary,
                })
                .collect(),
            pending,
        })
    }

    /// 保存中继配置并开始跟踪在线节点；已有的连接按新配置重连。
    /// 先把三样都核对过再落盘：有任何一样不对，原来的配置原样保留。
    pub fn configure_relay(
        self: &Arc<Self>,
        addr: String,
        token: Option<String>,
        fingerprint: Option<String>,
    ) -> Result<RelayView, String> {
        let addr = addr.trim().trim_end_matches('/').to_string();
        if !(addr.starts_with("wss://") || addr.starts_with("ws://")) {
            return Err("中继地址应以 wss:// 开头（放在反向代理后面、由代理加密时也可以是 ws://）".into());
        }
        // 没给指纹就沿用原来的；给了（哪怕是空的，表示改用正规证书）就按给的来
        let pin = match fingerprint.as_deref().map(str::trim) {
            None => None,
            Some("") => Some(String::new()),
            Some(text) => Some(Fingerprint::parse(text)?.to_string()),
        };
        let token = token.filter(|t| !t.trim().is_empty());
        if token.is_none() && self.secrets.get(ACCOUNT_RELAY_TOKEN)?.is_none() {
            return Err("缺少中继令牌：请粘贴中继启动时打印的完整连接串".into());
        }
        {
            let db = self.db.lock().unwrap();
            set_setting(&db, SETTINGS_RELAY_ADDR, &addr)?;
            if let Some(pin) = pin {
                set_setting(&db, SETTINGS_RELAY_FP, &pin)?;
            }
        }
        if let Some(token) = token {
            self.secrets.set(ACCOUNT_RELAY_TOKEN, &token)?;
        }
        self.start_watcher()?;
        {
            let db = self.db.lock().unwrap();
            events::record(&db, "relay.configured", "info", "中继", &format!("已设置中继 {addr}"), Some("#/machines"));
        }
        self.reconnect_all();
        relay_view(&self.db.lock().unwrap(), self.secrets.as_ref())
    }

    pub fn clear_relay(&self) -> Result<(), String> {
        {
            let db = self.db.lock().unwrap();
            set_setting(&db, SETTINGS_RELAY_ADDR, "")?;
            set_setting(&db, SETTINGS_RELAY_FP, "")?;
        }
        self.secrets.delete(ACCOUNT_RELAY_TOKEN)?;
        if let Some(watcher) = self.watcher.lock().unwrap().take() {
            watcher.stop();
        }
        let db = self.db.lock().unwrap();
        events::record(&db, "relay.cleared", "info", "中继", "已清除中继配置", None);
        Ok(())
    }

    pub fn set_install_script(&self, url: &str) -> Result<(), String> {
        let url = url.trim();
        if !url.is_empty() && !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err("安装脚本地址应以 http:// 或 https:// 开头".into());
        }
        let db = self.db.lock().unwrap();
        set_setting(&db, SETTINGS_INSTALL_SCRIPT, url)
    }

    /// 所有机器立刻按当前配置重连
    fn reconnect_all(&self) {
        for remote in self.nodes.lock().unwrap().values() {
            let _ = self.connect_now(&remote.id);
        }
    }

    fn get(&self, node_id: &str) -> Result<Arc<Remote>, String> {
        self.nodes
            .lock()
            .unwrap()
            .get(node_id)
            .cloned()
            .ok_or_else(|| format!("没有添加过机器「{node_id}」"))
    }

    pub fn link(&self, node_id: &str) -> Option<HubLink> {
        self.nodes
            .lock()
            .unwrap()
            .get(node_id)
            .and_then(|remote| remote.link.lock().unwrap().clone())
    }

    /// 机器不在线时，把最近一次的原因带出来
    pub fn offline_reason(&self, node_id: &str) -> String {
        match self.nodes.lock().unwrap().get(node_id) {
            Some(remote) => {
                let status = remote.status.lock().unwrap();
                match &status.error {
                    Some(error) => format!("机器「{}」不在线：{error}", remote.name()),
                    None => format!("机器「{}」不在线", remote.name()),
                }
            }
            None => format!("机器「{node_id}」不在线：还没有添加，或已被移除"),
        }
    }

    pub fn views(&self) -> Vec<NodeView> {
        let mut views: Vec<NodeView> = self
            .nodes
            .lock()
            .unwrap()
            .values()
            .map(|remote| remote.view())
            .collect();
        views.sort_by(|a, b| a.name.cmp(&b.name));
        views
    }

    pub fn view(&self, node_id: &str) -> Result<NodeView, String> {
        Ok(self.get(node_id)?.view())
    }

    /// 立刻重连：正在用的那条连接先让位，请求马上失败而不是排着等
    pub fn connect_now(&self, node_id: &str) -> Result<(), String> {
        let remote = self.get(node_id)?;
        remote.link.lock().unwrap().take();
        let _ = remote.kick.send_modify(|n| *n += 1);
        Ok(())
    }

    pub fn rename(&self, node_id: &str, name: &str) -> Result<NodeView, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("名字不能为空".into());
        }
        let remote = self.get(node_id)?;
        remote.cfg.lock().unwrap().name = name.to_string();
        let config = remote.cfg.lock().unwrap().clone();
        let info = remote.info.lock().unwrap().clone();
        upsert_node(&self.db.lock().unwrap(), node_id, &config, info.as_ref())?;
        self.node_sink.send(remote.event(None));
        Ok(remote.view())
    }

    /// 移除一台机器：断开连接，删掉配对密钥与记录。对端保存的密钥随之作废
    pub fn remove(&self, node_id: &str) -> Result<(), String> {
        let remote = self.get(node_id)?;
        let _ = remote.shutdown.send(true);
        remote.link.lock().unwrap().take();
        self.nodes.lock().unwrap().remove(node_id);
        self.secrets.delete(&secrets::node_psk_account(node_id))?;
        let db = self.db.lock().unwrap();
        db.execute("DELETE FROM nodes WHERE id = ?1", params![node_id])
            .map_err(|e| e.to_string())?;
        events::record(
            &db,
            "node.removed",
            "info",
            &remote.name(),
            "已移除机器，配对密钥已删除",
            Some("#/machines"),
        );
        drop(db);
        self.node_sink.send(remote.event(Some("已移除".into())));
        Ok(())
    }

    /// 用掉或作废邀请
    fn forget_invite(&self, code: Option<&str>, name: Option<&str>) -> Result<(), String> {
        let db = self.db.lock().unwrap();
        let invites = load_invites(&db);
        let kept: Vec<Invite> = invites
            .iter()
            .filter(|invite| code != Some(invite.code.as_str()) && name != Some(invite.name.as_str()))
            .cloned()
            .collect();
        if kept.len() != invites.len() {
            save_invites(&db, &kept)?;
        }
        Ok(())
    }

    fn drop_expired_invites(&self) -> Result<(), String> {
        let db = self.db.lock().unwrap();
        let invites = load_invites(&db);
        let kept: Vec<Invite> = invites.iter().filter(|invite| !invite.expired()).cloned().collect();
        if kept.len() != invites.len() {
            save_invites(&db, &kept)?;
        }
        Ok(())
    }

    /// 给新机器生成安装命令：里面带着一次性配对码，并记下一份邀请。
    /// 机器执行后连上中继，控制端凭这份邀请自动完成配对。
    pub fn install_command(&self, name: &str, allow: &[String]) -> Result<InstallCommand, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("请给这台机器起个名字（如 work-pc、home-mac、server）".into());
        }
        if name.chars().count() > 40 {
            return Err("名字太长了（最多 40 个字符）".into());
        }
        let (addr, pin, token) = {
            let db = self.db.lock().unwrap();
            relay_params(&db, self.secrets.as_ref())?
        };
        let code = secure::pairing_code();
        {
            // 同名的旧邀请作废：那台机器拿到的将是新的配对码
            let db = self.db.lock().unwrap();
            let mut invites: Vec<Invite> = load_invites(&db)
                .into_iter()
                .filter(|invite| !invite.expired() && invite.name != name)
                .collect();
            invites.push(Invite::new(name, code.clone()));
            save_invites(&db, &invites)?;
        }
        let script = setting(&self.db.lock().unwrap(), SETTINGS_INSTALL_SCRIPT)
            .unwrap_or_else(|| DEFAULT_INSTALL_SCRIPT.to_string());
        let mut command = format!("curl -fsSL {} | sh -s -- install", shell_quote(&script));
        for (flag, value) in [
            ("--name", name.to_string()),
            ("--relay", addr.clone()),
            ("--token", token),
            ("--pair", code.clone()),
        ] {
            command.push_str(&format!(" {flag} {}", shell_quote(&value)));
        }
        if let Some(pin) = pin {
            command.push_str(&format!(" --fp {}", shell_quote(&pin.to_string())));
        }
        for dir in allow.iter().map(|dir| dir.trim()).filter(|dir| !dir.is_empty()) {
            command.push_str(&format!(" --allow {}", shell_quote(dir)));
        }
        Ok(InstallCommand {
            name: name.to_string(),
            code,
            command,
            expires_in_s: PAIRING_TTL_S,
        })
    }

    /// 与一台机器配对：用配对码握手，换成长期密钥后加入管理。
    ///
    /// `direct`：直连时的（地址，证书指纹）；为空表示经中继。
    pub async fn pair(
        &self,
        node_id: &str,
        code: &str,
        name: Option<&str>,
        direct: Option<(String, Option<Fingerprint>)>,
    ) -> Result<NodeView, String> {
        let node_id = node_id.trim();
        let code = code.trim();
        if code.chars().filter(|c| c.is_ascii_alphanumeric()).count() < 12 {
            return Err("配对码不完整".into());
        }
        // 同一台机器同时只配一次（自动配对与手动配对可能撞上）
        if !self.pairing.lock().unwrap().insert(node_id.to_string()) {
            return Err("这台机器正在配对".into());
        }
        let paired = self.pair_inner(node_id, code, name, direct).await;
        self.pairing.lock().unwrap().remove(node_id);
        let remote = paired?;
        // 等连接任务登记完上线（取机器信息、Agent 清单）再返回，界面拿到的就是可用状态
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            {
                let status = remote.status.lock().unwrap();
                if status.online || status.error.is_some() || Instant::now() > deadline {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        Ok(remote.view())
    }

    /// 握手、换密钥、落库；返回这台机器的运行时状态（连接由连接任务负责）
    async fn pair_inner(
        &self,
        node_id: &str,
        code: &str,
        name: Option<&str>,
        direct: Option<(String, Option<Fingerprint>)>,
    ) -> Result<Arc<Remote>, String> {
        if self.nodes.lock().unwrap().contains_key(node_id) {
            return Err("这台机器已经添加过了".into());
        }
        let hub_id = hub_id(self.secrets.as_ref())?;
        let channel = match &direct {
            Some((addr, pin)) => net::dial(addr, pin.as_ref()).await?,
            None => {
                let (addr, pin, token) = {
                    let db = self.db.lock().unwrap();
                    relay_params(&db, self.secrets.as_ref())?
                };
                relay::open(&addr, pin.as_ref(), &token, node_id).await?
            }
        };
        // 新的长期密钥：握手完成后节点只认它
        let psk = secure::random_psk();
        let link = HubLink::handshake(
            channel,
            &hub_id,
            node_id,
            secure::psk_from_code(code),
            Some(psk),
        )
        .await?;
        // 节点已经换好密钥，这边必须存下来，否则再也连不上
        self.secrets
            .set(&secrets::node_psk_account(node_id), &URL_SAFE_NO_PAD.encode(psk))?;
        let info: Option<MachineInfo> = link
            .call("sys.info", json!({}))
            .await
            .ok()
            .and_then(|value| serde_json::from_value(value).ok());
        let name = name
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_string)
            .or_else(|| info.as_ref().map(|i| i.name.clone()))
            .unwrap_or_else(|| node_id.to_string());
        let config = match &direct {
            Some((addr, pin)) => NodeConfig {
                name,
                relay_url: String::new(),
                addr: addr.clone(),
                fingerprint: pin.as_ref().map(|p| p.to_string()).unwrap_or_default(),
            },
            None => {
                let db = self.db.lock().unwrap();
                NodeConfig {
                    name,
                    relay_url: setting(&db, SETTINGS_RELAY_ADDR).unwrap_or_default(),
                    addr: String::new(),
                    fingerprint: String::new(),
                }
            }
        };
        {
            let db = self.db.lock().unwrap();
            upsert_node(&db, node_id, &config, info.as_ref())?;
            events::record(
                &db,
                "node.paired",
                "info",
                &config.name,
                "已配对并添加这台机器",
                Some("#/machines"),
            );
        }
        let _ = self.forget_invite(Some(code), Some(&config.name));
        let remote = Remote::new(node_id.to_string(), config, info);
        self.nodes
            .lock()
            .unwrap()
            .insert(node_id.to_string(), Arc::clone(&remote));
        // 靠配对码连上来的这条连接到此为止：随即用长期密钥重连一次，
        // 顺便验证两边保存的密钥对得上（对不上的话现在就知道，而不是等到下次启动）
        link.close();
        tauri::async_runtime::spawn(self.supervisor.clone().run(Arc::clone(&remote), None));
        Ok(remote)
    }

    /// 让一台已配对的机器为另一台设备开放配对，返回配对码
    pub async fn authorize(&self, node_id: &str) -> Result<String, String> {
        let link = self
            .link(node_id)
            .ok_or_else(|| self.offline_reason(node_id))?;
        let code = secure::pairing_code();
        link.call("pair.allow", json!({ "code": code })).await?;
        let db = self.db.lock().unwrap();
        events::record(
            &db,
            "node.authorize",
            "info",
            &self.get(node_id)?.name(),
            "已为另一台设备开放配对（10 分钟内有效）",
            Some("#/machines"),
        );
        Ok(code)
    }
}

// ---------------------------------------------------------------------------
// 请求转发
// ---------------------------------------------------------------------------

/// 机器侧 git 命令的输出
pub type GitOut = autoagents_core::service::GitOutput;

/// 不同请求的等待时间
pub fn timeout_for(method: &str) -> Duration {
    match method {
        // 启动对话要等 Agent 进程拉起
        "turn.start" => Duration::from_secs(30),
        "turn.stop" => Duration::from_secs(10),
        // 推送大仓库、拉取慢镜像可能较慢（节点侧的上限是 120 秒）
        "git.run" => Duration::from_secs(150),
        // 节点侧可能要联网刷新模型目录
        "models.list" => Duration::from_secs(45),
        _ => autoagents_core::link::CALL_TIMEOUT,
    }
}

/// 向一台远程机器发请求并等结果
pub async fn call(
    state: &Arc<AppState>,
    node_id: &str,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let link = state
        .remote
        .link(node_id)
        .ok_or_else(|| state.remote.offline_reason(node_id))?;
    link.call_timeout(method, params, timeout_for(method))
        .await
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// 命令
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn relay_get(state: State<'_, Arc<AppState>>) -> Result<RelayView, String> {
    let db = state.db.lock().unwrap();
    relay_view(&db, state.secrets.as_ref())
}

/// 设置中继：粘贴中继启动时打印的连接串即可；也可以分开填地址、令牌、指纹。
/// 只填其中一部分时，其余沿用原来的。
pub fn set_relay(
    state: &Arc<AppState>,
    text: Option<&str>,
    addr: Option<&str>,
    token: Option<&str>,
    fingerprint: Option<&str>,
) -> Result<RelayView, String> {
    let parsed = parse_relay_input(text.unwrap_or(""));
    let existing = {
        let db = state.db.lock().unwrap();
        relay_view(&db, state.secrets.as_ref())?
    };
    let addr = non_empty(addr)
        .or(parsed.addr)
        .or_else(|| (!existing.addr.is_empty()).then(|| existing.addr.clone()))
        .ok_or("请粘贴中继启动时打印的「中继连接串」，或填写中继地址与令牌")?;
    let fingerprint = parsed.fingerprint.or_else(|| non_empty(fingerprint));
    let token = non_empty(token).or(parsed.token).or_else(|| {
        secrets_get(state.secrets.as_ref(), ACCOUNT_RELAY_TOKEN)
            .ok()
            .flatten()
    });
    state
        .remote
        .configure_relay(addr, token, fingerprint)
}

#[tauri::command]
pub fn relay_set(
    state: State<'_, Arc<AppState>>,
    text: Option<String>,
    addr: Option<String>,
    token: Option<String>,
    fingerprint: Option<String>,
) -> Result<RelayView, String> {
    set_relay(
        &state,
        text.as_deref(),
        addr.as_deref(),
        token.as_deref(),
        fingerprint.as_deref(),
    )
}

#[tauri::command]
pub fn relay_clear(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.remote.clear_relay()
}

#[tauri::command]
pub fn relay_status(state: State<'_, Arc<AppState>>) -> Result<RelayStatusView, String> {
    state.remote.relay_status()
}

#[tauri::command]
pub fn install_script_set(state: State<'_, Arc<AppState>>, url: String) -> Result<(), String> {
    state.remote.set_install_script(&url)
}

/// 给新机器生成安装命令（带一次性配对码）
#[tauri::command]
pub fn node_install_command(
    state: State<'_, Arc<AppState>>,
    name: String,
    allow: Vec<String>,
) -> Result<InstallCommand, String> {
    state.remote.install_command(&name, &allow)
}

/// 与中继上一台在线、尚未配对的机器配对
#[tauri::command]
pub async fn node_pair(
    state: State<'_, Arc<AppState>>,
    node_id: String,
    code: String,
    name: Option<String>,
) -> Result<NodeView, String> {
    state
        .remote
        .pair(&node_id, &code, name.as_deref(), None)
        .await
}

/// 直连配对：粘贴那台机器上 `agentd pair` 打印的连接串
#[tauri::command]
pub async fn node_pair_direct(
    state: State<'_, Arc<AppState>>,
    text: String,
) -> Result<NodeView, String> {
    let (addr, pin, node_id, name, code) = parse_node_input(&text)?;
    state
        .remote
        .pair(&node_id, &code, name.as_deref(), Some((addr, pin)))
        .await
}

/// 让一台已配对的机器为另一台设备开放配对
#[tauri::command]
pub async fn node_authorize(
    state: State<'_, Arc<AppState>>,
    node_id: String,
) -> Result<String, String> {
    state.remote.authorize(&node_id).await
}

#[tauri::command]
pub fn nodes_list(state: State<'_, Arc<AppState>>) -> Vec<NodeView> {
    state.remote.views()
}

#[tauri::command]
pub fn node_remove(state: State<'_, Arc<AppState>>, node_id: String) -> Result<(), String> {
    state.remote.remove(&node_id)
}

#[tauri::command]
pub fn node_rename(
    state: State<'_, Arc<AppState>>,
    node_id: String,
    name: String,
) -> Result<NodeView, String> {
    state.remote.rename(&node_id, &name)
}

/// 立刻重连某台机器
#[tauri::command]
pub fn node_connect_now(state: State<'_, Arc<AppState>>, node_id: String) -> Result<(), String> {
    state.remote.connect_now(&node_id)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::chat;
    use crate::testutil::{fake_script, service_for, test_state, TestState};
    use autoagents_core::link::{self, NodeAuth, NodeState};
    use autoagents_core::net::Identity;
    use autoagents_core::service::Service;

    const TOKEN: &str = "test-relay-token";

    /// 一台「远程」机器上的机器侧服务，机器名 home-pc
    fn fake_agent(script: &str) -> Arc<Service> {
        service_for(fake_script("claude", script), Some("home-pc"))
    }

    fn state(service: Arc<Service>) -> TestState {
        test_state(service)
    }

    /// 内存里起一个真实的中继（TCP + TLS）
    async fn start_relay() -> (String, Fingerprint, watch::Receiver<Vec<NodeSummary>>) {
        let dir = tempfile::tempdir().unwrap();
        let identity = Identity::load_or_create(dir.path()).unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(identity.server_config().unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("wss://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let (tx, rx) = watch::channel(Vec::new());
        let relay: Arc<autoagents_core::relay::TlsRelay> = autoagents_core::relay::Relay::new(TOKEN, tx);
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
        (url, identity.fingerprint(), rx)
    }

    /// 一台「远程」机器：报到中继，并把每个控制端连接接成一条加密连接。
    /// 任务被中止时，这些连接一并断开（与 agentd 退出时的效果一致）。
    fn node(
        url: &str,
        fp: &Fingerprint,
        node_id: &str,
        name: &str,
        auth: Arc<NodeState>,
        service: Arc<Service>,
    ) -> tokio::task::JoinHandle<()> {
        let (url, fp, node_id, name) = (url.to_string(), fp.clone(), node_id.to_string(), name.to_string());
        tokio::spawn(async move {
            let (incoming, mut rx) = mpsc::unbounded_channel::<String>();
            let control = relay::node_control(&url, Some(&fp), TOKEN, &node_id, &name, {
                move |conn_id: String| {
                    let _ = incoming.send(conn_id);
                }
            });
            tokio::pin!(control);
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _ = &mut control => break,
                    Some(conn_id) = rx.recv() => {
                        let (url, fp, auth, service) =
                            (url.clone(), fp.clone(), auth.clone(), service.clone());
                        connections.spawn(async move {
                            if let Ok(channel) = relay::join(&url, Some(&fp), TOKEN, &conn_id).await {
                                let _ = link::serve(channel, auth, service).await;
                            }
                        });
                    }
                }
            }
        })
    }

    async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !cond() {
            assert!(std::time::Instant::now() < deadline, "等待超时：{what}");
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }

    async fn wait_offline(state: &Arc<AppState>, id: &str) {
        wait_until("机器离线", || {
            state.remote.view(id).is_ok_and(|view| !view.online)
        })
        .await;
    }

    /// 等中继把这台机器列出来（它的控制连接是异步建立的）
    async fn wait_listed(state: &Arc<AppState>, id: &str) {
        wait_until("中继列出机器", || {
            state
                .remote
                .relay_status()
                .is_ok_and(|status| status.connected && status.online.iter().any(|n| n.summary.node_id == id))
        })
        .await;
    }

    async fn wait_online(state: &Arc<AppState>, id: &str) -> NodeView {
        wait_until("机器上线", || state.remote.view(id).is_ok_and(|view| view.online)).await;
        state.remote.view(id).unwrap()
    }

    const OK: &str = r#"printf '%s\n' '{"type":"result","is_error":false,"result":"远程机器上的回答"}'"#;

    /// 粘贴中继的连接串 → 生成安装命令 → 机器执行后连上中继、自动配对 →
    /// 请求转发 → 在远程机器上跑一轮对话
    #[tokio::test]
    async fn install_command_pairs_the_machine_automatically() {
        let (url, fp, _online) = start_relay().await;
        let service = fake_agent(OK);
        let auth = NodeState::new("node-1".into(), HashMap::new(), None);
        let machine = node(&url, &fp, "node-1", "home-pc", Arc::clone(&auth), Arc::clone(&service));
        let TestState { state, secrets, nodes: collector, .. } = state(Arc::clone(&service));

        // 没配中继时给不出安装命令
        assert!(state.remote.install_command("home-pc", &[]).unwrap_err().contains("还没有设置中继"));
        assert!(state.remote.offline_reason("node-1").contains("不在线"));

        // 粘贴中继的连接串
        let connect = format!("autoagents://relay?addr={url}&token={TOKEN}&fp={fp}");
        let view = set_relay(&state, Some(&connect), None, None, None).unwrap();
        assert_eq!((view.addr.as_str(), view.fingerprint.as_str()), (url.as_str(), fp.to_string().as_str()));
        assert!(view.configured && view.has_token);
        assert!(!view.install_script.is_empty());

        // 安装命令带着配对码、中继地址与令牌、允许访问的目录
        let command = state.remote.install_command("home-pc", &["/home/me".into()]).unwrap();
        assert!(command.command.contains("install --name 'home-pc'"));
        assert!(command.command.contains(&format!("--relay '{url}'")));
        assert!(command.command.contains(&format!("--fp '{}'", fp)));
        assert!(command.command.contains(&format!("--token '{TOKEN}'")));
        assert!(command.command.contains("--allow '/home/me'"));
        assert!(!command.command.contains("autoagents://"));
        assert_eq!(command.expires_in_s, PAIRING_TTL_S);
        {
            let status = state.remote.relay_status().unwrap();
            assert_eq!(status.pending.len(), 1, "{status:?}");
            assert_eq!(status.pending[0].name, "home-pc");
            assert!(status.pending[0].expires_in_s > PAIRING_TTL_S - 5);
        }

        // 机器已经连上中继，但还没拿到配对码：配不上，也不该被添加
        wait_until("中继列出机器", || {
            let status = state.remote.relay_status().unwrap();
            status.connected && status.online.len() == 1
        })
        .await;
        {
            let status = state.remote.relay_status().unwrap();
            assert_eq!(status.online[0].summary.name, "home-pc");
            assert!(!status.online[0].paired);
        }
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(state.remote.views().is_empty(), "机器没有开放配对时不该配上");

        // 机器执行了安装命令、开始等配对：控制端自动配上
        auth.allow_pairing(&command.code);
        let online = wait_online(&state, "node-1").await;
        assert_eq!(online.info.as_ref().unwrap().name, "home-pc");
        assert!(online.status.rtt_ms.is_some());
        assert!(state.remote.relay_status().unwrap().pending.is_empty(), "用掉的邀请要清掉");

        // 长期密钥存好了，之后凭它重连
        let stored = secrets.get(&secrets::node_psk_account("node-1")).unwrap().unwrap();
        assert!(decode_psk(&stored).is_some());
        assert_eq!(auth.paired_count(), 1);
        assert!(!auth.is_pairing_open(), "配对码只能用一次");
        // 同一台机器不能重复添加
        assert!(state.remote.pair("node-1", &command.code, None, None).await.unwrap_err().contains("已经添加过"));

        // 请求转发
        let info = call(&state, "node-1", "sys.info", json!({})).await.unwrap();
        assert_eq!(info["name"], "home-pc");
        assert!(call(&state, "node-1", "nope", json!({})).await.unwrap_err().contains("不支持请求"));
        assert!(call(&state, "ghost", "sys.info", json!({})).await.unwrap_err().contains("不在线"));

        // 在远程机器上跑一轮对话
        let id = chat::start(&state, Some("node-1"), "claude-cli", None, None, "hi").await.unwrap();
        wait_until("远程对话结束", || !state.live.lock().unwrap().contains_key(&id)).await;
        {
            let db = state.db.lock().unwrap();
            let (role, content): (String, String) = db
                .query_row(
                    "SELECT role, content FROM messages WHERE session_id = ?1 ORDER BY id DESC LIMIT 1",
                    params![id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!((role.as_str(), content.as_str()), ("assistant", "远程机器上的回答"));
        }
        // 状态变化都通知到了界面
        let events = collector.0.lock().unwrap();
        assert!(events.iter().any(|e| e.id == "node-1" && e.online), "{events:?}");
        assert!(events.iter().all(|e| e.name == "home-pc"));
        drop(events);
        machine.abort();
    }

    /// 机器掉线：状态变离线并说明原因；它上面正在运行的会话以失败收尾；恢复后自动重连。
    /// 在线时也可以要求立即重连。
    #[tokio::test]
    async fn going_offline_ends_running_sessions_and_it_reconnects() {
        let (url, fp, _online) = start_relay().await;
        let service = fake_agent("sleep 30");
        let auth = NodeState::new("node-1".into(), HashMap::new(), None);
        auth.allow_pairing("ABCD-EFGH-JKLM-NPQR");
        let machine = node(&url, &fp, "node-1", "home-pc", Arc::clone(&auth), Arc::clone(&service));
        let TestState { state, .. } = state(Arc::clone(&service));
        state.remote.configure_relay(url.clone(), Some(TOKEN.into()), Some(fp.to_string())).unwrap();
        wait_listed(&state, "node-1").await;
        // 手动配对：返回时已经是可用状态
        let paired = state.remote.pair("node-1", "ABCD-EFGH-JKLM-NPQR", None, None).await.unwrap();
        assert!(paired.online, "{paired:?}");
        assert_eq!(paired.status.error, None);

        // 在线时立即重连：正在跑的对话随旧连接结束，随后回到在线
        let id = chat::start(&state, Some("node-1"), "claude-cli", None, None, "slow").await.unwrap();
        wait_until("对话在远程机器上启动", || service.running_turns() == 1).await;
        state.remote.connect_now("node-1").unwrap();
        assert!(state.remote.link("node-1").is_none(), "旧连接先让位");
        wait_until("重连后对话以失败收尾", || !state.live.lock().unwrap().contains_key(&id)).await;
        wait_until("旧连接上的对话被停掉", || service.running_turns() == 0).await;
        let again = wait_online(&state, "node-1").await;
        assert!(again.status.error.is_none(), "{again:?}");

        // 机器下线：它上面正在运行的对话以失败收尾，原因留在状态里
        let slow = chat::start(&state, Some("node-1"), "claude-cli", None, None, "slow").await.unwrap();
        wait_until("对话在远程机器上启动", || service.running_turns() == 1).await;
        machine.abort();
        wait_offline(&state, "node-1").await;
        let view = state.remote.view("node-1").unwrap();
        assert!(view.status.error.is_some(), "{view:?}");
        assert!(view.status.last_seen_ms.is_some());
        wait_until("会话以失败收尾", || !state.live.lock().unwrap().contains_key(&slow)).await;
        let db = state.db.lock().unwrap();
        let (role, content): (String, String) = db
            .query_row(
                "SELECT role, content FROM messages WHERE session_id = ?1 ORDER BY id DESC LIMIT 1",
                params![slow],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(role, "system");
        assert!(content.contains("连接断开"), "{content}");
        drop(db);
        // 离线的机器上发不了消息，且不会留下用户消息
        assert!(chat::send(&state, slow, "again").await.unwrap_err().contains("不在线"));
        assert!(state.remote.authorize("node-1").await.unwrap_err().contains("不在线"));
        let count: i64 = state
            .db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM messages WHERE session_id = ?1", params![slow], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2, "离线时的消息不该落库");

        // 机器回来后自动重连
        let machine = node(&url, &fp, "node-1", "home-pc", Arc::clone(&auth), Arc::clone(&service));
        wait_online(&state, "node-1").await;
        machine.abort();
    }

    /// 移除机器：断开、删密钥与记录；重命名；为另一台设备开放配对
    #[tokio::test]
    async fn remove_rename_and_authorize() {
        let (url, fp, _online) = start_relay().await;
        let service = fake_agent(OK);
        let auth = NodeState::new("node-1".into(), HashMap::new(), None);
        auth.allow_pairing("ABCD-EFGH-JKLM-NPQR");
        let machine = node(&url, &fp, "node-1", "home-pc", Arc::clone(&auth), Arc::clone(&service));
        let TestState { state, secrets, nodes: collector, .. } = state(Arc::clone(&service));
        state.remote.configure_relay(url.clone(), Some(TOKEN.into()), Some(fp.to_string())).unwrap();
        wait_listed(&state, "node-1").await;
        state.remote.pair("node-1", "ABCD-EFGH-JKLM-NPQR", None, None).await.unwrap();
        wait_online(&state, "node-1").await;

        // 重命名
        let renamed = state.remote.rename("node-1", "家里的 Mac").unwrap();
        assert_eq!(renamed.name, "家里的 Mac");
        assert!(state.remote.rename("node-1", "  ").is_err());

        // 为另一台设备开放配对
        state.remote.authorize("node-1").await.unwrap();
        assert!(auth.is_pairing_open());

        // 移除
        collector.0.lock().unwrap().clear();
        state.remote.remove("node-1").unwrap();
        assert!(secrets.get(&secrets::node_psk_account("node-1")).unwrap().is_none());
        assert!(state.remote.views().is_empty());
        assert!(state.remote.link("node-1").is_none());
        {
            let db = state.db.lock().unwrap();
            let count: i64 = db.query_row("SELECT count(*) FROM nodes", [], |row| row.get(0)).unwrap();
            assert_eq!(count, 0);
            let kinds: Vec<String> = {
                let mut stmt = db.prepare("SELECT kind FROM events ORDER BY id").unwrap();
                stmt.query_map([], |row| row.get(0)).unwrap().flatten().collect()
            };
            assert_eq!(kinds, ["relay.configured", "node.paired", "node.online", "node.authorize", "node.removed"]);
        }
        assert_eq!(collector.0.lock().unwrap().last().unwrap().message.as_deref(), Some("已移除"));
        assert!(state.remote.remove("node-1").unwrap_err().contains("没有添加过"));
        machine.abort();
    }

    /// 中继状态：在线的机器、是否已配对、等着接入的邀请；过期的邀请不再列出
    #[tokio::test]
    async fn relay_status_lists_online_machines() {
        let (url, fp, _online) = start_relay().await;
        let service = fake_agent(OK);
        let auth = NodeState::new("node-1".into(), HashMap::new(), None);
        let machine = node(&url, &fp, "node-1", "home-pc", Arc::clone(&auth), Arc::clone(&service));
        let TestState { state, .. } = state(Arc::clone(&service));
        state.remote.configure_relay(url.clone(), Some(TOKEN.into()), Some(fp.to_string())).unwrap();
        state.remote.install_command("server", &[]).unwrap();
        // 早该过期的邀请
        {
            let db = state.db.lock().unwrap();
            let stale = Invite {
                name: "old-pc".into(),
                code: "OLD0-OLD0-OLD0-OLD0".into(),
                created_ms: now_ms() - (PAIRING_TTL_S + 60) * 1000,
            };
            let mut invites = load_invites(&db);
            invites.push(stale);
            save_invites(&db, &invites).unwrap();
        }
        wait_until("中继连上并列出机器", || {
            state.remote.relay_status().is_ok_and(|s| s.connected && s.online.len() == 1)
        })
        .await;
        let status = state.remote.relay_status().unwrap();
        assert!(status.message.is_none(), "{status:?}");
        assert_eq!(status.online[0].summary.name, "home-pc");
        assert!(!status.online[0].paired);
        // 名字对不上的邀请不会触发自动配对
        assert!(state.remote.views().is_empty());
        let names: Vec<&str> = status.pending.iter().map(|invite| invite.name.as_str()).collect();
        assert_eq!(names, ["server"]);
        machine.abort();
    }

    /// 解析：中继连接串、裸地址、节点的直连连接串；只改一部分时其余沿用
    #[test]
    fn parse_inputs_and_partial_relay_settings() {
        assert_eq!(
            parse_relay_input("autoagents://relay?addr=wss://1.2.3.4:7000&token=abc&fp=sha256:00"),
            ParsedRelay {
                addr: Some("wss://1.2.3.4:7000".into()),
                token: Some("abc".into()),
                fingerprint: Some("sha256:00".into())
            }
        );
        assert_eq!(parse_relay_input(" wss://relay.example.com "), ParsedRelay { addr: Some("wss://relay.example.com".into()), ..Default::default() });
        assert_eq!(parse_relay_input(""), ParsedRelay::default());

        let full = format!("sha256:{}", "ab".repeat(32));
        let text = format!("autoagents://node?addr=wss://5.6.7.8:7100&fp={full}&id=node-9&name=home%20mac&pair=ABCD-EFGH-JKLM-NPQR");
        let (addr, pin, id, name, code) = parse_node_input(&text).unwrap();
        assert_eq!(addr, "wss://5.6.7.8:7100");
        assert_eq!(pin.map(|p| p.to_string()), Some(full.clone()));
        assert!(parse_node_input(&format!("autoagents://node?addr=wss://x&id=y&pair=ABCD-EFGH-JKLM-NPQR&fp=sha256:ab")).is_err());
        // loopback 上的明文直连也接受
        let plain = parse_node_input("autoagents://node?addr=ws://127.0.0.1:7100&id=n&pair=ABCD-EFGH-JKLM-NPQR").unwrap();
        assert_eq!((plain.0.as_str(), plain.1, plain.2.as_str()), ("ws://127.0.0.1:7100", None, "n"));
        assert_eq!((id.as_str(), name.as_deref(), code.as_str()), ("node-9", Some("home mac"), "ABCD-EFGH-JKLM-NPQR"));
        assert!(parse_node_input("autoagents://node?addr=wss://x&id=y").unwrap_err().contains("配对码"));
        assert!(parse_node_input("wss://x").unwrap_err().contains("应以 autoagents://node?"));

        assert_eq!(percent_decode("a%20b%E4%B8%AD%E6%96%87%zz"), "a b中文%zz");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");

        let service = fake_agent(OK);
        let TestState { state, secrets, .. } = state(service);
        assert!(set_relay(&state, Some(""), None, None, None).unwrap_err().contains("中继连接串"));
        let connect = "autoagents://relay?addr=wss://127.0.0.1:17001&token=first-token&fp=sha256:0011223344556677889900112233445566778899001122334455667788990011";
        let view = set_relay(&state, Some(connect), None, None, None).unwrap();
        assert_eq!(view.addr, "wss://127.0.0.1:17001");
        assert!(view.configured);
        assert_eq!(secrets.get(ACCOUNT_RELAY_TOKEN).unwrap().as_deref(), Some("first-token"));
        // 只改地址：令牌与指纹沿用
        let view = set_relay(&state, None, Some("wss://127.0.0.1:17002/"), None, None).unwrap();
        assert_eq!(view.addr, "wss://127.0.0.1:17002");
        assert!(view.has_token);
        assert!(view.fingerprint.starts_with("sha256:00112233"));
        // 指纹不对：整份都不保存
        assert!(set_relay(&state, Some(""), Some("wss://127.0.0.1:17003"), None, Some("nope")).is_err());
        assert_eq!(relay_view(&state.db.lock().unwrap(), secrets.as_ref()).unwrap().addr, "wss://127.0.0.1:17002");
        // 清除
        state.remote.clear_relay().unwrap();
        assert!(!state.remote.relay_configured());
        assert!(secrets.get(ACCOUNT_RELAY_TOKEN).unwrap().is_none());
    }
}
