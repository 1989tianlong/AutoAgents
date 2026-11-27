//! 控制端与节点之间的端到端加密：中继只看得到密文，篡改与重放都会被发现。
//!
//! 握手是 Noise `NNpsk2` 的等价构造，自己实现、足够小、便于审计：
//!
//! ```text
//! 控制端                                  节点
//!   hello   { hubId, nodeId, ephHub }  ──▶
//!   helloOk { ephNode, mac }           ◀──
//!   confirm { mac }                    ──▶   此后双方都确认对方持有 psk
//! ```
//!
//! 双方各出一个一次性 X25519 密钥对做 DH，再拌入只有两端知道的预共享密钥 `psk`：
//!
//! ```text
//! transcript = SHA-256(协议名 ‖ hubId ‖ nodeId ‖ ephHub ‖ ephNode)
//! ck  = HMAC(key = transcript, msg = DH)        ← Noise 的 MixKey
//! ck  = HMAC(key = ck,        msg = psk)
//! k_mac ‖ k_控制端→节点 ‖ k_节点→控制端 = HKDF-Expand(ck, 协议名 ‖ transcript)
//! ```
//!
//! 不知道 psk 的一方（包括中继）算不出任何一把密钥，也就伪造不出 mac；
//! 知道 psk 但不在这条连接上的一方拿不到一次性私钥，同样算不出来。
//!
//! 首次配对时 psk 由配对码派生（强度有限），握手完成后控制端立即经加密通道下发
//! 一个新的随机 psk（`rotate`），配对码随之作废，之后的连接都用新 psk。
//! 握手之后每条消息都是 ChaCha20-Poly1305 密文，带严格递增的序号，重放会被拒绝。

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, Zeroizing};

/// 预共享密钥：只有控制端与这一个节点知道
pub type Psk = [u8; 32];

const LABEL: &[u8] = b"AutoAgents-HS-v1";
/// 配对码派生 psk 时的域分隔，和握手里的密钥派生不是同一把密钥
const CODE_LABEL: &[u8] = b"AutoAgents-pair-v1";
/// 密文帧头部的序号长度
const COUNTER: usize = 8;
/// ChaCha20-Poly1305 的认证标签长度
const TAG: usize = 16;

/// 系统安全随机数
pub fn random<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).expect("系统随机数不可用");
    buf
}

pub fn random_psk() -> Psk {
    random()
}

/// 配对码用的字母表：去掉了容易看错的 0 / O / 1 / I / L
const CODE_ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";

/// 生成一个配对码：16 位约 79 比特，`XXXX-XXXX-XXXX-XXXX`（拒绝采样消除取模偏差）
pub fn pairing_code() -> String {
    let limit = (256 / CODE_ALPHABET.len()) * CODE_ALPHABET.len(); // 248 = 31 * 8
    let mut code = String::with_capacity(16);
    while code.len() < 16 {
        let raw: [u8; 32] = random();
        for byte in raw {
            if (byte as usize) < limit {
                code.push(CODE_ALPHABET[byte as usize % CODE_ALPHABET.len()] as char);
                if code.len() == 16 {
                    break;
                }
            }
        }
    }
    format!(
        "{}-{}-{}-{}",
        &code[0..4],
        &code[4..8],
        &code[8..12],
        &code[12..16]
    )
}

/// 规范化并校验配对码：去除空白和连字符后必须至少 12 位字母/数字（避免 `"--------"` 退化成空密钥）。
pub fn normalize_pairing_code(code: &str) -> Result<Zeroizing<String>, String> {
    let normalized: String = code
        .trim()
        .chars()
        .filter(|c| *c != '-' && *c != ' ')
        .flat_map(|c| c.to_uppercase())
        .collect();
    if normalized.len() < 12 || !normalized.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("配对码长度不足或包含非法字符（去掉连字符后至少 12 位）".into());
    }
    Ok(Zeroizing::new(normalized))
}

/// 配对码 → 握手用的 psk。配对码不区分大小写、忽略首尾空白与连字符（手工输入容错）。
pub fn psk_from_code(code: &str) -> Psk {
    let normalized = Zeroizing::new(
        code.trim()
            .chars()
            .filter(|c| *c != '-' && *c != ' ')
            .flat_map(|c| c.to_uppercase())
            .collect::<String>(),
    );
    hmac(CODE_LABEL, normalized.as_bytes())
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn unb64(text: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| "编码不合法".to_string())
}

fn key32(text: &str) -> Result<[u8; 32], String> {
    unb64(text)?.try_into().map_err(|_| "长度不对".to_string())
}

fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC 接受任意长度的密钥");
    mac.update(data);
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&mac.finalize().into_bytes());
    bytes
}

/// 长度前缀避免「ab‖c」与「a‖bc」撞成同一份记录
fn transcript(hub_id: &str, node_id: &str, eph_hub: &[u8], eph_node: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(LABEL);
    for part in [hub_id.as_bytes(), node_id.as_bytes(), eph_hub, eph_node] {
        hash.update((part.len() as u32).to_be_bytes());
        hash.update(part);
    }
    hash.finalize().into()
}

struct Keys {
    mac: [u8; 32],
    hub_to_node: [u8; 32],
    node_to_hub: [u8; 32],
}

impl Drop for Keys {
    fn drop(&mut self) {
        self.mac.zeroize();
        self.hub_to_node.zeroize();
        self.node_to_hub.zeroize();
    }
}

/// `secret` / `other` 是参与 DH 的私钥与对方公钥（哪一方调用都一样）。
/// 拒绝低阶点（RFC 7748 §6.1），并对中间密钥材料立即清零。
fn derive(
    secret: &StaticSecret,
    other: &[u8; 32],
    psk: &[u8],
    th: &[u8; 32],
) -> Result<Keys, String> {
    let dh = secret.diffie_hellman(&PublicKey::from(*other));
    if !dh.was_contributory() {
        return Err("对方的临时公钥无效（低阶点），拒绝握手".into());
    }
    let shared = Zeroizing::new(dh.to_bytes());
    let ck1 = Zeroizing::new(hmac(th, &*shared));
    let ck = Zeroizing::new(hmac(&*ck1, psk));
    let hk = Hkdf::<Sha256>::from_prk(&*ck).expect("链密钥正好是一个哈希的长度");
    let mut okm = Zeroizing::new([0u8; 96]);
    hk.expand(LABEL, &mut *okm).expect("输出长度合法");
    let (mac, rest) = okm.split_at(32);
    let (hub_to_node, node_to_hub) = rest.split_at(32);
    Ok(Keys {
        mac: mac.try_into().expect("32 字节"),
        hub_to_node: hub_to_node.try_into().expect("32 字节"),
        node_to_hub: node_to_hub.try_into().expect("32 字节"),
    })
}

/// 控制端 → 节点的第一句话
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub hub_id: String,
    pub node_id: String,
    pub eph: String,
}

/// 节点 → 控制端的应答，附带证明自己持有 psk 的 mac
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloOk {
    pub eph: String,
    pub mac: String,
}

/// 控制端 → 节点的确认，证明自己持有 psk
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Confirm {
    pub mac: String,
}

/// 握手的发起方（控制端）。一次性的，用完即弃。
pub struct Initiator {
    secret: StaticSecret,
    eph: [u8; 32],
    psk: Psk,
    hub_id: String,
    node_id: String,
}

impl Drop for Initiator {
    fn drop(&mut self) {
        self.psk.zeroize();
    }
}

impl Initiator {
    pub fn new(psk: Psk, hub_id: impl Into<String>, node_id: impl Into<String>) -> Self {
        let secret = StaticSecret::from(random());
        Self {
            eph: PublicKey::from(&secret).to_bytes(),
            secret,
            psk,
            hub_id: hub_id.into(),
            node_id: node_id.into(),
        }
    }

    pub fn hello(&self) -> Hello {
        Hello {
            hub_id: self.hub_id.clone(),
            node_id: self.node_id.clone(),
            eph: b64(&self.eph),
        }
    }

    /// 用节点的应答算出密钥；对方拿错了 psk 或应答被改过都会被拒绝。
    pub fn finish(self, reply: &HelloOk) -> Result<(Cipher, Confirm), String> {
        let their = key32(&reply.eph)?;
        let th = transcript(&self.hub_id, &self.node_id, &self.eph, &their);
        let keys = derive(&self.secret, &their, &self.psk, &th)?;
        if !equal(&hmac(&keys.mac, b"node"), &key32(&reply.mac)?) {
            return Err("节点身份校验失败：预共享密钥不匹配，或数据在中途被改过".into());
        }
        Ok((
            Cipher::hub(&keys),
            Confirm {
                mac: b64(&hmac(&keys.mac, b"hub")),
            },
        ))
    }
}

/// 握手的应答方（节点）
pub struct Responder {
    eph: [u8; 32],
    keys: Keys,
}

impl Responder {
    /// 校验发起方公钥并派生会话密钥；拒绝非法编码或低阶点。
    pub fn try_new(mut psk: Psk, hello: &Hello) -> Result<Self, String> {
        let secret = StaticSecret::from(random());
        let eph = PublicKey::from(&secret).to_bytes();
        let their = key32(&hello.eph)?;
        let th = transcript(&hello.hub_id, &hello.node_id, &their, &eph);
        let keys = derive(&secret, &their, &psk, &th);
        psk.zeroize();
        Ok(Self { eph, keys: keys? })
    }

    /// 兼容旧调用的构造：非法公钥时直接返回全零假密钥（`complete` 必然失败）。
    pub fn new(psk: Psk, hello: &Hello) -> Self {
        Self::try_new(psk, hello).unwrap_or_else(|_| Self {
            eph: random(),
            keys: Keys {
                mac: random(),
                hub_to_node: random(),
                node_to_hub: random(),
            },
        })
    }

    pub fn reply(&self) -> HelloOk {
        HelloOk {
            eph: b64(&self.eph),
            mac: b64(&hmac(&self.keys.mac, b"node")),
        }
    }

    /// 校验控制端的确认；通过之前不要处理它的任何请求。
    pub fn complete(self, confirm: &Confirm) -> Result<Cipher, String> {
        if !equal(&hmac(&self.keys.mac, b"hub"), &key32(&confirm.mac)?) {
            return Err("控制端身份校验失败：拒绝连接".into());
        }
        Ok(Cipher::node(&self.keys))
    }
}

/// 常数时间比较
fn equal(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.ct_eq(b).into()
}

/// 握手完成后的加密通道。两个方向各有独立的密钥与序号。
#[derive(Debug)]
pub struct Cipher {
    send: ChaCha20Poly1305,
    recv: ChaCha20Poly1305,
    send_counter: u64,
    recv_counter: u64,
}

impl Cipher {
    fn hub(keys: &Keys) -> Self {
        Self::new(keys.hub_to_node, keys.node_to_hub)
    }

    fn node(keys: &Keys) -> Self {
        Self::new(keys.node_to_hub, keys.hub_to_node)
    }

    fn new(send: [u8; 32], recv: [u8; 32]) -> Self {
        Self {
            send: ChaCha20Poly1305::new((&send).into()),
            recv: ChaCha20Poly1305::new((&recv).into()),
            send_counter: 0,
            recv_counter: 0,
        }
    }

    /// 加密一条消息：`序号(8) ‖ 密文 ‖ 标签(16)`，序号参与认证（序号耗尽时拒绝加密防 nonce 重用）
    pub fn try_seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, String> {
        let next = self
            .send_counter
            .checked_add(1)
            .ok_or_else(|| "会话密钥序号已耗尽，请重新建立连接".to_string())?;
        let counter = self.send_counter;
        let mut frame = counter.to_be_bytes().to_vec();
        let sealed = self
            .send
            .encrypt(
                &Nonce::from(nonce(counter)),
                Payload {
                    msg: plaintext,
                    aad: &frame,
                },
            )
            .map_err(|_| "加密消息失败".to_string())?;
        frame.extend_from_slice(&sealed);
        self.send_counter = next;
        Ok(frame)
    }

    /// 加密一条消息（便捷方法）
    pub fn seal(&mut self, plaintext: &[u8]) -> Vec<u8> {
        self.try_seal(plaintext).expect("会话序号未耗尽")
    }

    /// 解密一条消息。序号必须与期望值完全一致：重放、乱序、对端重启都会被拒绝。
    pub fn open(&mut self, frame: &[u8]) -> Result<Vec<u8>, String> {
        if frame.len() < COUNTER + TAG {
            return Err("密文过短".into());
        }
        let next = self
            .recv_counter
            .checked_add(1)
            .ok_or_else(|| "接收序号已耗尽，请重新建立连接".to_string())?;
        let (head, body) = frame.split_at(COUNTER);
        let counter = u64::from_be_bytes(head.try_into().expect("8 字节"));
        if counter != self.recv_counter {
            return Err(format!(
                "消息序号不对（期望 {}，收到 {counter}）：连接被重放或两边状态已不一致",
                self.recv_counter
            ));
        }
        let plain = self
            .recv
            .decrypt(
                &Nonce::from(nonce(counter)),
                Payload {
                    msg: body,
                    aad: head,
                },
            )
            .map_err(|_| "解密失败：数据被篡改，或两边的密钥不一致".to_string())?;
        self.recv_counter = next;
        Ok(plain)
    }
}

fn nonce(counter: u64) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[4..].copy_from_slice(&counter.to_be_bytes());
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 走完一遍握手。`node_psk` 可以和 `hub_psk` 不同，用来模拟密钥不匹配。
    fn handshake(hub_psk: Psk, node_psk: Psk) -> Result<(Cipher, Cipher), String> {
        let initiator = Initiator::new(hub_psk, "hub-1", "node-1");
        let responder = Responder::new(node_psk, &initiator.hello());
        let (hub, confirm) = initiator.finish(&responder.reply())?;
        let node = responder.complete(&confirm)?;
        Ok((hub, node))
    }

    /// 两端算出相同的密钥，双向都能加解密；多条消息序号递增
    #[test]
    fn handshake_agrees_and_encrypts_both_ways() {
        let psk = random_psk();
        let (mut hub, mut node) = handshake(psk, psk).unwrap();
        let f1 = hub.seal(b"one");
        let f2 = hub.seal(b"two");
        assert_eq!(node.open(&f1).unwrap(), b"one");
        assert_eq!(node.open(&f2).unwrap(), b"two");
        assert_eq!(
            hub.open(&node.seal("结果".as_bytes())).unwrap(),
            "结果".as_bytes()
        );
        // 密文里看不到明文
        assert!(!f1.windows(3).any(|w| w == b"one"));
        assert_eq!(f1.len(), 8 + 3 + 16);
    }

    /// 控制端拿错了 psk：节点的应答验不过，拿不到任何密钥
    #[test]
    fn wrong_psk_is_rejected_by_hub() {
        let err = handshake(random_psk(), random_psk()).unwrap_err();
        assert!(err.contains("预共享密钥不匹配"), "{err}");
    }

    /// 不知道 psk 的一方冒充控制端：能收到应答，但过不了确认这一关
    #[test]
    fn forged_confirm_is_rejected_by_node() {
        let psk = random_psk();
        let initiator = Initiator::new(psk, "hub-1", "node-1");
        let responder = Responder::new(psk, &initiator.hello());
        assert!(
            responder
                .complete(&Confirm {
                    mac: b64(&random::<32>())
                })
                .is_err()
        );
        // 拿「配对码错了」的 psk 也一样
        let responder = Responder::new(random_psk(), &initiator.hello());
        assert!(initiator.finish(&responder.reply()).is_err());
    }

    /// 应答在中途被换掉（中间人换成自己的临时公钥）：拒绝
    #[test]
    fn tampered_reply_is_rejected() {
        let psk = random_psk();
        let initiator = Initiator::new(psk, "hub-1", "node-1");
        let honest = Responder::new(psk, &initiator.hello());
        let mut reply = honest.reply();
        // 中间人替换临时公钥，但拿不出对应的 mac
        reply.eph = b64(&random::<32>());
        assert!(initiator.finish(&reply).is_err());
    }

    /// 中继把这次握手转给「另一个节点 id」也不行：双方标识绑定在密钥里
    #[test]
    fn identities_are_bound_into_the_keys() {
        let psk = random_psk();
        let honest = Initiator::new(psk, "hub-1", "node-1");
        let node = Responder::new(psk, &honest.hello());
        let reply = node.reply();
        // 冒充者对节点自称在找 node-2，拿 node-1 的应答来用
        let impostor = Initiator::new(psk, "hub-1", "node-2");
        assert!(impostor.finish(&reply).is_err());
        assert!(honest.finish(&reply).is_ok());
    }

    /// 密文或序号被改、被重放：都会被发现
    #[test]
    fn tampered_replayed_and_misordered_frames_are_rejected() {
        let psk = random_psk();
        let (mut hub, mut node) = handshake(psk, psk).unwrap();
        let frame = hub.seal(b"secret");
        let mut tampered = frame.clone();
        tampered[12] ^= 0x01;
        assert!(node.open(&tampered).unwrap_err().contains("解密失败"));
        let mut header = frame.clone();
        header[0] ^= 0x01;
        assert!(node.open(&header).is_err());
        assert!(node.open(&frame).is_ok());
        // 同一帧再发一次：重放
        assert!(node.open(&frame).unwrap_err().contains("消息序号不对"));
        // 跳着发：乱序（后发的先到）
        let (mut hub2, mut node2) = handshake(psk, psk).unwrap();
        let _first = hub2.seal(b"a");
        let second = hub2.seal(b"b");
        assert!(node2.open(&second).unwrap_err().contains("消息序号不对"));
        // 大消息与残缺帧
        assert!(node.open(&hub.seal(&vec![0u8; 70_000])).unwrap().len() == 70_000);
        assert!(node.open(&[0u8; 10]).unwrap_err().contains("密文过短"));
        assert!(node.open(&hub.seal(b"")).unwrap().is_empty());
    }

    /// 两个方向密钥不同：自己发的帧自己解不开
    #[test]
    fn directions_have_independent_keys() {
        let psk = random_psk();
        let (mut hub, _node) = handshake(psk, psk).unwrap();
        let frame = hub.seal(b"x");
        assert!(hub.open(&frame).unwrap_err().contains("解密失败"));
    }

    /// 配对码不区分大小写、忽略连字符与空白；两边的 psk 一致
    #[test]
    fn pairing_code_derives_the_same_psk_on_both_sides() {
        let a = psk_from_code("K7QM-2X9P-4R8T-6WYZ");
        let b = psk_from_code("  k7qm 2x9p-4r8t-6wyz ");
        assert_eq!(a, b);
        assert_ne!(a, psk_from_code("K7QM-2X9P-4R8T-6WYX"));
        let (mut hub, mut node) = handshake(a, b).unwrap();
        assert_eq!(
            node.open(&hub.seal("配对成功".as_bytes())).unwrap(),
            "配对成功".as_bytes()
        );
    }

    /// 握手消息是线上格式：字段名稳定、能原样往返
    #[test]
    fn handshake_messages_round_trip_on_the_wire() {
        let hello = Initiator::new(random_psk(), "hub", "node").hello();
        let wire = serde_json::to_value(&hello).unwrap();
        assert_eq!(wire["hubId"], "hub");
        assert_eq!(wire["nodeId"], "node");
        assert_eq!(wire["eph"].as_str().unwrap().len(), 43);
        let back: Hello = serde_json::from_value(wire).unwrap();
        assert_eq!(back, hello);
    }

    #[test]
    fn pairing_codes_look_right_and_differ() {
        let a = pairing_code();
        let b = pairing_code();
        assert_ne!(a, b);
        assert_eq!(a.len(), 19);
        assert!(
            a.chars()
                .all(|c| c == '-' || CODE_ALPHABET.contains(&(c as u8))),
            "{a}"
        );
    }

    #[test]
    fn random_bytes_differ() {
        assert_ne!(random_psk(), random_psk());
    }
}
