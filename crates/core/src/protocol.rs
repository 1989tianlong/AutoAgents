//! 控制端 ↔ 节点的线上消息（JSON）。
//!
//! 握手阶段明文交换 [`Frame::Hello`] / [`Frame::HelloOk`] / [`Frame::Confirm`]，
//! 之后的每一帧都走加密通道（见 [`crate::secure`]），在应用看来只是普通的 JSON。

use crate::runner::TurnEvent;
use crate::secure::{Confirm, Hello, HelloOk};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 请求 / 应答 / 握手 / 保活
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Frame {
    #[serde(rename = "hello")]
    Hello(Hello),
    #[serde(rename = "hello_ok")]
    HelloOk(HelloOk),
    #[serde(rename = "confirm")]
    Confirm(Confirm),
    /// 握手或配对被拒绝
    #[serde(rename = "reject")]
    Reject {
        message: String,
    },
    /// 首次配对后换成长期密钥（base64）。只应出现在加密通道里
    #[serde(rename = "rotate")]
    Rotate {
        psk: String,
    },
    #[serde(rename = "rotated")]
    Rotated {},
    #[serde(rename = "request")]
    Request {
        id: u64,
        method: String,
        #[serde(default)]
        params: Value,
    },
    #[serde(rename = "response")]
    Response {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    #[serde(rename = "ping")]
    Ping {
        nonce: u64,
    },
    #[serde(rename = "pong")]
    Pong {
        nonce: u64,
    },
}

impl Frame {
    pub fn request(id: u64, method: &str, params: Value) -> Self {
        Frame::Request {
            id,
            method: method.to_string(),
            params,
        }
    }

    pub fn ok(id: u64, result: Value) -> Self {
        Frame::Response {
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: u64, message: impl Into<String>) -> Self {
        Frame::Response {
            id,
            result: None,
            error: Some(message.into()),
        }
    }
}

/// 连接上传输的一条消息：要么是 [`Frame`]，要么是机器侧主动推来的对话事件
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Wire {
    Frame(Frame),
    Event(TurnEvent),
}

impl From<Frame> for Wire {
    fn from(frame: Frame) -> Self {
        Wire::Frame(frame)
    }
}

impl From<TurnEvent> for Wire {
    fn from(event: TurnEvent) -> Self {
        Wire::Event(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 线上格式是控制端与节点之间的约定：标签与字段名必须稳定
    #[test]
    fn frame_tags_are_stable() {
        let cases = [
            (
                r#"{"type":"hello","hubId":"h","nodeId":"n","eph":"e"}"#,
                Frame::Hello(Hello { hub_id: "h".into(), node_id: "n".into(), eph: "e".into() }),
            ),
            (
                r#"{"type":"hello_ok","eph":"e","mac":"m"}"#,
                Frame::HelloOk(HelloOk { eph: "e".into(), mac: "m".into() }),
            ),
            (
                r#"{"type":"confirm","mac":"m"}"#,
                Frame::Confirm(Confirm { mac: "m".into() }),
            ),
            (
                r#"{"type":"request","id":7,"method":"sys.info"}"#,
                Frame::Request {
                    id: 7,
                    method: "sys.info".into(),
                    params: Value::Null,
                },
            ),
            (
                r#"{"type":"response","id":7,"result":{"ok":true}}"#,
                Frame::ok(7, serde_json::json!({ "ok": true })),
            ),
            (r#"{"type":"response","id":7,"error":"boom"}"#, Frame::err(7, "boom")),
            (r#"{"type":"ping","nonce":3}"#, Frame::Ping { nonce: 3 }),
        ];
        for (text, frame) in cases {
            assert_eq!(serde_json::from_str::<Frame>(text).unwrap(), frame, "{text}");
            let back = serde_json::to_string(&frame).unwrap();
            assert_eq!(serde_json::from_str::<Frame>(&back).unwrap(), frame, "往返：{back}");
        }
        // 错误应答不带 result，成功应答不带 error
        assert_eq!(
            serde_json::to_string(&Frame::err(1, "x")).unwrap(),
            r#"{"type":"response","id":1,"error":"x"}"#
        );
    }

    /// 对话事件与普通帧共用一条连接：按 type 分流
    #[test]
    fn wire_separates_frames_from_events() {
        let event = Wire::Event(TurnEvent::Delta {
            turn_id: "1".into(),
            text: "hi".into(),
        });
        let text = serde_json::to_string(&event).unwrap();
        assert!(text.contains(r#""type":"turn.delta""#), "{text}");
        assert!(matches!(
            serde_json::from_str::<Wire>(&text).unwrap(),
            Wire::Event(TurnEvent::Delta { .. })
        ));
        let frame = Wire::Frame(Frame::Ping { nonce: 1 });
        let text = serde_json::to_string(&frame).unwrap();
        assert!(matches!(
            serde_json::from_str::<Wire>(&text).unwrap(),
            Wire::Frame(Frame::Ping { nonce: 1 })
        ));
    }
}
