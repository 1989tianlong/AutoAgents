//! 传输层：WebSocket（可选 TLS）+ 端到端加密，包成一条收发 JSON 消息的 [`Channel`]。
//!
//! - 远程地址一律要求 TLS：自签名证书按指纹（SHA-256）钉住，或走正常 CA 校验
//! - 明文 `ws://` 只允许连本机回环地址（本地测试、容器内通信）
//! - 握手完成后调用 [`Channel::encrypt`]，此后的每一帧都是带序号的 AEAD 密文

use crate::protocol::Wire;
use crate::secure::Cipher;
use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::http::Request;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use url::Url;

/// 单条消息的上限：一轮对话的完整回答、git 输出都在这之内。
/// 中继原样转发密文（比明文多 24 字节），所以留出余量
pub const MAX_MESSAGE: usize = 16 * 1024 * 1024;

/// 建连、握手的超时
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// 空闲连接的发保活间隔
pub const PING_INTERVAL: Duration = Duration::from_secs(30);

/// 指纹的文本形式：`sha256:` + 64 位十六进制
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint(pub [u8; 32]);

impl Fingerprint {
    pub fn of(der: &[u8]) -> Self {
        Self(Sha256::digest(der).into())
    }

    /// 解析 `sha256:abcd…`（十六进制不区分大小写，忽略其中的冒号）
    pub fn parse(text: &str) -> Result<Self, String> {
        let hex: String = text
            .trim()
            .trim_start_matches("sha256:")
            .chars()
            .filter(|c| *c != ':')
            .collect();
        let bytes = hex::decode(&hex).filter(|b| b.len() == 32).ok_or_else(|| {
            format!("证书指纹不合法（应为 sha256: 加 64 位十六进制）：{text}")
        })?;
        Ok(Self(bytes.try_into().expect("32 字节")))
    }
}

impl std::fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sha256:{}", hex::encode(&self.0))
    }
}

mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn decode(text: &str) -> Option<Vec<u8>> {
        if text.len() % 2 != 0 || !text.is_ascii() {
            return None;
        }
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
            .collect()
    }
}

/// 服务端的身份：一张自签名证书与私钥，首次启动时生成，之后一直沿用
pub struct Identity {
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
}

impl Identity {
    /// 从 `dir/cert.der`、`dir/key.der` 读取；不存在就生成（私钥文件权限 0600）。
    pub fn load_or_create(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建目录 {} 失败：{e}", dir.display()))?;
        let cert_path = dir.join("cert.der");
        let key_path = dir.join("key.der");
        let load = |path: &Path| -> Result<Option<Vec<u8>>, String> {
            match std::fs::read(path) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(format!("读取 {} 失败：{e}", path.display())),
            }
        };
        let (cert, key) = match (load(&cert_path)?, load(&key_path)?) {
            (Some(cert), Some(key)) if !cert.is_empty() && !key.is_empty() => (cert, key),
            _ => {
                let generated = rcgen::generate_simple_self_signed(vec!["autoagents".into()])
                    .map_err(|e| format!("生成证书失败：{e}"))?;
                let cert = generated.cert.der().to_vec();
                let key = generated.signing_key.serialize_der();
                // 私钥先落盘且只给本人可读，再写证书：中断在中间也只会留下「重新生成」的结果
                write_private(&key_path, &key)?;
                std::fs::write(&cert_path, &cert)
                    .map_err(|e| format!("写入 {} 失败：{e}", cert_path.display()))?;
                (cert, key)
            }
        };
        Ok(Self {
            cert: CertificateDer::from(cert),
            key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
        })
    }

    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::of(self.cert.as_ref())
    }

    pub fn server_config(&self) -> Result<Arc<rustls::ServerConfig>, String> {
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![self.cert.clone()], self.key.clone_key())
            .map_err(|e| format!("证书与私钥不匹配：{e}"))?;
        Ok(Arc::new(config))
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

/// 只认指定指纹的证书校验器：自签名证书不需要 CA，指纹对上即可信。
/// 首次配对时用户核对过指纹，之后每次连接都钉住它，中间人换证书会被发现。
#[derive(Debug)]
struct PinVerifier {
    expected: [u8; 32],
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if Sha256::digest(end_entity.as_ref()).as_slice() == self.expected {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "证书指纹不匹配：对端的证书变了（可能被冒充，或对方重新生成过证书）".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn client_config(pin: Option<&Fingerprint>) -> Result<Arc<rustls::ClientConfig>, String> {
    let config = match pin {
        Some(pin) => {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let verifier = Arc::new(PinVerifier {
                expected: pin.0,
                provider: provider.clone(),
            });
            rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map_err(|e| e.to_string())?
                .dangerous()
                .with_custom_certificate_verifier(verifier)
                .with_no_client_auth()
        }
        // 没给指纹：按正常 CA 校验（中继放在正规证书的反向代理后面时用）
        None => {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth()
        }
    };
    Ok(Arc::new(config))
}

/// `Sec-WebSocket-Key`：16 字节随机数的 base64
fn b64_key() -> String {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    STANDARD.encode(crate::secure::random::<16>())
}

fn ws_config() -> tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
    tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE))
}

/// 客户端一侧的底层流：明文 TCP 或 TLS
pub enum Stream {
    Plain(TcpStream),
    Tls(tokio_rustls::client::TlsStream<TcpStream>),
}

impl AsyncRead for Stream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match &mut *self {
            Stream::Plain(stream) => std::pin::Pin::new(stream).poll_read(cx, buf),
            Stream::Tls(stream) => std::pin::Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match &mut *self {
            Stream::Plain(stream) => std::pin::Pin::new(stream).poll_write(cx, buf),
            Stream::Tls(stream) => std::pin::Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match &mut *self {
            Stream::Plain(stream) => std::pin::Pin::new(stream).poll_flush(cx),
            Stream::Tls(stream) => std::pin::Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match &mut *self {
            Stream::Plain(stream) => std::pin::Pin::new(stream).poll_shutdown(cx),
            Stream::Tls(stream) => std::pin::Pin::new(stream).poll_shutdown(cx),
        }
    }
}

/// 一条连接上的收发端。握手阶段明文，之后加密。
pub struct Channel<S> {
    ws: WebSocketStream<S>,
    cipher: Option<Cipher>,
}

impl<S> std::fmt::Debug for Channel<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 不输出任何内容相关的字段
        f.debug_struct("Channel")
            .field("encrypted", &self.cipher.is_some())
            .finish()
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> Channel<S> {
    pub fn new(ws: WebSocketStream<S>) -> Self {
        Self { ws, cipher: None }
    }

    pub fn encrypt(&mut self, cipher: Cipher) {
        self.cipher = Some(cipher);
    }

    pub fn is_encrypted(&self) -> bool {
        self.cipher.is_some()
    }

    /// 发一条消息（对话协议）
    pub async fn send(&mut self, wire: &Wire) -> Result<(), String> {
        self.send_json(wire).await
    }

    /// 发任意 JSON 消息（中继协议等）
    pub async fn send_json<T: serde::Serialize>(&mut self, value: &T) -> Result<(), String> {
        let plain = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        let message = match &mut self.cipher {
            Some(cipher) => Message::Binary(cipher.seal(&plain).into()),
            None => Message::Text(String::from_utf8(plain).map_err(|e| e.to_string())?.into()),
        };
        self.ws
            .send(message)
            .await
            .map_err(|e| format!("发送失败：{e}"))
    }

    /// 收一条消息（对话协议）。连接断开或数据有问题时返回 Err，之后这条连接不应再用。
    pub async fn recv(&mut self) -> Result<Wire, String> {
        self.recv_json().await
    }

    /// 收一条 JSON 消息并解析成 `T`
    pub async fn recv_json<T: serde::de::DeserializeOwned>(&mut self) -> Result<T, String> {
        loop {
            let message = match self.ws.next().await {
                Some(Ok(message)) => message,
                Some(Err(e)) => return Err(format!("连接出错：{e}")),
                None => return Err("连接已断开".into()),
            };
            let payload = match (message, &mut self.cipher) {
                // WebSocket 的 Ping / Pong / Close 由底层处理，不交给上层
                (Message::Ping(_) | Message::Pong(_), _) => continue,
                (Message::Close(_), _) => return Err("对端关闭了连接".into()),
                (Message::Text(text), None) => text.as_bytes().to_vec(),
                (Message::Binary(data), Some(cipher)) => match cipher.open(&data) {
                    Ok(plain) => plain,
                    Err(e) => return Err(e),
                },
                // 加密之后还来明文、加密之前就来二进制：都算协议错误
                _ => return Err("协议错误：消息类型与加密状态不符".into()),
            };
            return serde_json::from_slice::<T>(&payload)
                .map_err(|e| format!("无法解析消息（{} 字节）：{e}", payload.len()));
        }
    }

    /// 在两条连接之间原样转发消息，直到任何一方断开。
    /// 中继用它转发密文：不解析、不解密，也就不可能看到内容。
    pub async fn pipe<T: AsyncRead + AsyncWrite + Unpin>(&mut self, other: &mut Channel<T>) -> Result<(), String> {
        loop {
            tokio::select! {
                message = self.ws.next() => {
                    match message {
                        Some(Ok(message)) if matches!(message, Message::Text(_) | Message::Binary(_)) => {
                            other.ws.send(message).await.map_err(|e| format!("转发失败：{e}"))?;
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => return Err(format!("连接出错：{e}")),
                        None => return Err("连接已断开".into()),
                    }
                }
                message = other.ws.next() => {
                    match message {
                        Some(Ok(message)) if matches!(message, Message::Text(_) | Message::Binary(_)) => {
                            self.ws.send(message).await.map_err(|e| format!("转发失败：{e}"))?;
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => return Err(format!("连接出错：{e}")),
                        None => return Err("连接已断开".into()),
                    }
                }
            }
        }
    }

    /// 礼貌地关闭连接（通知对端），不等待对端回应
    pub async fn close(&mut self) {
        let _ = self.ws.close(None).await;
    }
}

/// 主动连接一个 WebSocket 地址。
pub async fn dial(url: &str, pin: Option<&Fingerprint>) -> Result<Channel<Stream>, String> {
    let url = Url::parse(url.trim()).map_err(|_| format!("地址不合法：{url}"))?;
    let secure = match url.scheme() {
        "wss" => true,
        "ws" => false,
        other => return Err(format!("不支持的协议「{other}」，请用 wss:// 或 ws://")),
    };
    let host = url.host_str().ok_or("地址里没有主机名")?.to_string();
    let port = url.port_or_known_default().unwrap_or(if secure { 443 } else { 80 });
    if !secure && !is_loopback(&host) {
        return Err("远程地址必须使用 wss://（明文 ws:// 只允许连接本机）".into());
    }
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), port)))
        .await
        .map_err(|_| format!("连接 {host}:{port} 超时"))?
        .map_err(|e| format!("无法连接 {host}:{port}：{e}"))?;
    let stream = if secure {
        let name = rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|_| format!("主机名不合法：{host}"))?
            .to_owned();
        let connector = tokio_rustls::TlsConnector::from(client_config(pin)?);
        let tls = tokio::time::timeout(CONNECT_TIMEOUT, connector.connect(name, stream))
            .await
            .map_err(|_| format!("TLS 握手超时：{host}:{port}"))?
            .map_err(|e| format!("TLS 握手失败：{e}"))?;
        Stream::Tls(tls)
    } else {
        Stream::Plain(stream)
    };
    let mut path = url.path().to_string();
    if let Some(query) = url.query() {
        path.push('?');
        path.push_str(query);
    }
    let host_header = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.clone(),
    };
    // client_async 要求请求行是绝对地址（ws://host/path），从中校验 Host
    let mut target = format!("{}://{host_header}{path}", if secure { "wss" } else { "ws" });
    if let Some(query) = url.query() {
        target.push('?');
        target.push_str(query);
    }
    // 握手头由这里补齐（client_async 不代劳）：随机挑战串防缓存代理误判
    let request = Request::builder()
        .uri(target)
        .header("Host", host_header)
        .header("User-Agent", "AutoAgents")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", b64_key())
        .body(())
        .map_err(|e| e.to_string())?;
    let (ws, response) = tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio_tungstenite::client_async_with_config(request, stream, Some(ws_config())),
    )
    .await
    .map_err(|_| format!("WebSocket 握手超时：{host}:{port}"))?
    .map_err(|e| format!("WebSocket 握手失败：{e}"))?;
    if response.status() != tokio_tungstenite::tungstenite::http::StatusCode::SWITCHING_PROTOCOLS {
        return Err(format!("WebSocket 握手被拒绝：{}", response.status()));
    }
    Ok(Channel::new(ws))
}

/// 接受一个已经建好的 TCP 连接（服务端）
pub async fn accept<S: AsyncRead + AsyncWrite + Unpin>(stream: S) -> Result<Channel<S>, String> {
    let ws = tokio_tungstenite::accept_async_with_config(stream, Some(ws_config()))
        .await
        .map_err(|e| format!("WebSocket 握手失败：{e}"))?;
    Ok(Channel::new(ws))
}

/// `accept_with_health` 在已经回过普通 HTTP 应答时返回的错误
pub const PLAIN_HTTP: &str = "plain-http";

/// 请求头的读取上限：WebSocket 握手请求只有几百字节
const MAX_HEAD: usize = 16 * 1024;

/// 带着一段「已经读过」的字节的流：读完这段再读底层流。
/// 服务端为了辨认请求类型先读了请求头，这部分数据要原样交回 WebSocket 实现。
pub struct Prefixed<S> {
    prefix: Vec<u8>,
    offset: usize,
    inner: S,
}

impl<S> Prefixed<S> {
    fn new(inner: S, prefix: Vec<u8>) -> Self {
        Self {
            prefix,
            offset: 0,
            inner,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Prefixed<S> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = &mut *self;
        if this.offset < this.prefix.len() {
            let remaining = &this.prefix[this.offset..];
            let n = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..n]);
            this.offset += n;
            return std::task::Poll::Ready(Ok(()));
        }
        std::pin::Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Prefixed<S> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// 服务端：接受一条连接并完成 WebSocket 握手。
///
/// 来的不是 WebSocket 请求（负载均衡的健康检查、浏览器误访问）时，回一个 200 纯文本
/// `health` 并返回 [`PLAIN_HTTP`]。WebSocket 实现在握手时不给「先看一眼再决定」的机会，
/// 所以请求头由这里先读出来辨认，再原样交回去。
pub async fn accept_with_health<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    health: &str,
) -> Result<Channel<Prefixed<S>>, String> {
    let prefixed = peek_request(stream, health).await?;
    let ws = tokio_tungstenite::accept_async_with_config(prefixed, Some(ws_config()))
        .await
        .map_err(|e| format!("WebSocket 握手失败：{e}"))?;
    Ok(Channel::new(ws))
}

/// 读出请求头：是 WebSocket 升级请求就把读过的字节连同流一起交回；
/// 否则直接回健康检查的应答。
async fn peek_request<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    health: &str,
) -> Result<Prefixed<S>, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let head_end = loop {
        if let Some(position) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            break position;
        }
        if buf.len() > MAX_HEAD {
            return Err("请求头过大，不像 WebSocket 握手".into());
        }
        let mut chunk = [0u8; 1024];
        let read = tokio::time::timeout(CONNECT_TIMEOUT, stream.read(&mut chunk))
            .await
            .map_err(|_| "等待请求超时".to_string())?
            .map_err(|e| format!("读取请求失败：{e}"))?;
        if read == 0 {
            return Err("对端还没发请求就断开了".into());
        }
        buf.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
    // Upgrade: websocket，且 Connection 里列了 upgrade（可能还带着 keep-alive 等其它选项）
    let header = |name: &str| -> Option<String> {
        head.lines().find_map(|line| {
            let rest = line.trim_start().strip_prefix(name)?;
            rest.starts_with([':', ' ', '\t'])
                .then(|| rest.trim_start_matches([':', ' ', '\t']).trim().to_string())
        })
    };
    let wants_upgrade = header("connection")
        .is_some_and(|value| value.split(',').any(|token| token.trim() == "upgrade"));
    if wants_upgrade && header("upgrade").as_deref() == Some("websocket") {
        return Ok(Prefixed::new(stream, buf));
    }
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{health}",
        health.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
    Err(PLAIN_HTTP.to_string())
}

/// 接受一个 TCP 连接并完成 TLS 与 WebSocket 握手（服务端）。
/// TLS 之后的普通 HTTP 请求同 [`accept_with_health`]。
pub async fn accept_tls_with_health(
    stream: TcpStream,
    acceptor: tokio_rustls::TlsAcceptor,
    health: &str,
) -> Result<Channel<Prefixed<tokio_rustls::server::TlsStream<TcpStream>>>, String> {
    let tls = tokio::time::timeout(CONNECT_TIMEOUT, acceptor.accept(stream))
        .await
        .map_err(|_| "TLS 握手超时".to_string())?
        .map_err(|e| format!("TLS 握手失败：{e}"))?;
    accept_with_health(tls, health).await
}

/// 接受一个 TCP 连接并完成 TLS（服务端）
pub async fn accept_tls(
    stream: TcpStream,
    acceptor: tokio_rustls::TlsAcceptor,
) -> Result<Channel<tokio_rustls::server::TlsStream<TcpStream>>, String> {
    let tls = tokio::time::timeout(CONNECT_TIMEOUT, acceptor.accept(stream))
        .await
        .map_err(|_| "TLS 握手超时".to_string())?
        .map_err(|e| format!("TLS 握手失败：{e}"))?;
    accept(tls).await
}

pub fn is_loopback(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.trim_matches(['[', ']'])
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_round_trips() {
        let fp = Fingerprint([0xab; 32]);
        let text = fp.to_string();
        assert!(text.starts_with("sha256:abab"));
        assert_eq!(Fingerprint::parse(&text).unwrap(), fp);
        // 带冒号的写法（openssl 风格）与大小写都不影响
        let colon = Fingerprint([0xab; 32]).to_string().replace("sha256:", "sha256:").chars().collect::<String>();
        let mut grouped = String::from("sha256:");
        for (i, b) in colon["sha256:".len()..].as_bytes().chunks(2).enumerate() {
            if i > 0 {
                grouped.push(':');
            }
            grouped.push_str(std::str::from_utf8(b).unwrap());
        }
        assert_eq!(Fingerprint::parse(&grouped).unwrap(), Fingerprint([0xab; 32]));
        assert!(Fingerprint::parse("sha256:abcd").is_err());
        assert!(Fingerprint::parse("nope").is_err());
    }

    #[test]
    fn identity_is_stable_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let a = Identity::load_or_create(dir.path()).unwrap();
        let b = Identity::load_or_create(dir.path()).unwrap();
        assert_eq!(a.fingerprint(), b.fingerprint(), "证书应当只生成一次");
        assert!(a.server_config().is_ok());
        let other = tempfile::tempdir().unwrap();
        assert_ne!(a.fingerprint(), Identity::load_or_create(other.path()).unwrap().fingerprint());
    }

    #[test]
    fn loopback_detection() {
        assert!(is_loopback("localhost"));
        assert!(is_loopback("127.0.0.1"));
        assert!(is_loopback("::1"));
        assert!(!is_loopback("192.168.1.2"));
        assert!(!is_loopback("example.com"));
    }

    /// 普通的 HTTP 请求得到健康检查应答；WebSocket 握手不受影响（Connection 里带别的选项也认）
    #[tokio::test]
    async fn plain_http_gets_a_health_answer() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            assert_eq!(
                accept_with_health(stream, "healthy\n").await.unwrap_err(),
                PLAIN_HTTP
            );
        });
        let mut tcp = TcpStream::connect(addr).await.unwrap();
        tcp.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        tcp.read_to_string(&mut response).await.unwrap();
        assert!(
            response.starts_with("HTTP/1.1 200 OK") && response.ends_with("healthy\n"),
            "{response}"
        );

        let (client_io, server_io) = tokio::io::duplex(1 << 16);
        tokio::spawn(async move {
            let mut channel = accept_with_health(server_io, "x").await.unwrap();
            let wire = channel.recv().await.unwrap();
            channel.send(&wire).await.unwrap();
        });
        let request = Request::builder()
            .uri("ws://node/")
            .header("Host", "node")
            .header("Connection", "keep-alive, Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", b64_key())
            .body(())
            .unwrap();
        let (ws, _) = tokio_tungstenite::client_async(request, client_io).await.unwrap();
        let mut client = Channel::new(ws);
        client
            .send(&Wire::Frame(crate::protocol::Frame::Ping { nonce: 1 }))
            .await
            .unwrap();
        assert_eq!(
            client.recv().await.unwrap(),
            Wire::Frame(crate::protocol::Frame::Ping { nonce: 1 })
        );
        server.await.unwrap();
    }

    /// 明文回环连接上收发消息
    #[tokio::test]
    async fn plain_loopback_round_trip() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut channel = accept(stream).await.unwrap();
            let wire = channel.recv().await.unwrap();
            channel.send(&wire).await.unwrap();
        });
        let mut client = dial(&format!("ws://{addr}"), None).await.unwrap();
        assert!(!client.is_encrypted());
        client
            .send(&Wire::Frame(crate::protocol::Frame::Ping { nonce: 42 }))
            .await
            .unwrap();
        assert_eq!(
            client.recv().await.unwrap(),
            Wire::Frame(crate::protocol::Frame::Ping { nonce: 42 })
        );
        server.await.unwrap();
    }

    /// TLS：指纹对上才能连，指纹不对会被拒绝
    #[tokio::test]
    async fn tls_requires_the_pinned_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let identity = Identity::load_or_create(dir.path()).unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(identity.server_config().unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    if let Ok(mut channel) = accept_tls(stream, acceptor).await {
                        let _ = channel.close().await;
                    }
                });
            }
        });
        let good = dial(&format!("wss://localhost:{}", addr.port()), Some(&identity.fingerprint())).await;
        assert!(good.is_ok(), "{good:?}");
        let wrong = dial(&format!("wss://localhost:{}", addr.port()), Some(&Fingerprint([9; 32]))).await;
        assert!(wrong.unwrap_err().contains("指纹"), "指纹不匹配应当被拒绝");
        // 明文连不上 TLS 端口；非回环地址不允许明文
        assert!(dial(&format!("ws://localhost:{}", addr.port()), None).await.is_err());
        assert!(dial("ws://10.0.0.1:7000", None).await.unwrap_err().contains("wss://"));
        assert!(dial("http://localhost:1", None).await.is_err());
        server.abort();
    }
}
