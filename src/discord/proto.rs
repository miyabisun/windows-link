//! Discord's local RPC wire format: 8-byte frames (opcode and length, little-endian)
//! carrying JSON, commands matched to replies by nonce, and events.

use serde_json::{Value, json};

pub const OP_HANDSHAKE: u32 = 0;
pub const OP_FRAME: u32 = 1;
pub const OP_CLOSE: u32 = 2;
pub const OP_PING: u32 = 3;
pub const OP_PONG: u32 = 4;

/// Discord's messages are small; anything larger is a broken stream.
pub const MAX_FRAME: usize = 1 << 20;

pub fn encode(op: u32, payload: &Value) -> Vec<u8> {
    let body = payload.to_string().into_bytes();
    let mut frame = Vec::with_capacity(8 + body.len());
    frame.extend_from_slice(&op.to_le_bytes());
    frame.extend_from_slice(&u32::try_from(body.len()).unwrap_or(u32::MAX).to_le_bytes());
    frame.extend_from_slice(&body);
    frame
}

/// Opcode and payload length from a frame header.
pub fn header(bytes: [u8; 8]) -> Result<(u32, usize), String> {
    let op = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let len = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    if len > MAX_FRAME {
        return Err(format!("frame of {len} bytes is too large"));
    }
    Ok((op, len))
}

pub fn handshake(client_id: &str) -> Value {
    json!({ "v": 1, "client_id": client_id })
}

pub fn command(cmd: &str, args: &Value, evt: Option<&str>, nonce: &str) -> Value {
    let mut message = json!({ "cmd": cmd, "args": args, "nonce": nonce });
    if let Some(evt) = evt {
        message["evt"] = json!(evt);
    }
    message
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Discord error {}: {}", self.code, self.message)
    }
}

/// Discord's code when the user turns down an authorization request.
pub const CODE_REJECTED: i64 = 5000;
/// Codes for an access token Discord no longer accepts.
pub const CODE_INVALID_TOKEN: [i64; 2] = [4006, 4009];

#[derive(Debug, PartialEq)]
pub enum Incoming {
    Reply {
        nonce: String,
        result: Result<Value, RpcError>,
    },
    Event {
        evt: String,
        data: Value,
    },
}

/// Sort a received frame into a reply to one of our commands or an event.
pub fn classify(message: &Value) -> Option<Incoming> {
    let error = || RpcError {
        code: message["data"]["code"].as_i64().unwrap_or_default(),
        message: message["data"]["message"]
            .as_str()
            .unwrap_or("unknown error")
            .to_owned(),
    };
    if let Some(nonce) = message["nonce"].as_str() {
        let result = if message["evt"] == "ERROR" {
            Err(error())
        } else {
            Ok(message["data"].clone())
        };
        return Some(Incoming::Reply {
            nonce: nonce.to_owned(),
            result,
        });
    }
    let evt = message["evt"].as_str()?;
    Some(Incoming::Event {
        evt: evt.to_owned(),
        data: message["data"].clone(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Incoming, OP_FRAME, OP_HANDSHAKE, RpcError, classify, command, encode, header};

    #[test]
    fn frames_carry_the_opcode_and_length_little_endian() {
        let frame = encode(OP_HANDSHAKE, &json!({ "v": 1, "client_id": "42" }));
        let body = br#"{"client_id":"42","v":1}"#;
        assert_eq!(&frame[..4], &[0, 0, 0, 0]);
        assert_eq!(
            &frame[4..8],
            &u32::try_from(body.len()).unwrap().to_le_bytes()
        );
        assert_eq!(&frame[8..], body);
        let head: [u8; 8] = encode(OP_FRAME, &json!({}))[..8].try_into().unwrap();
        assert_eq!(header(head).unwrap(), (OP_FRAME, 2));
    }

    #[test]
    fn rejects_oversized_frames() {
        let mut head = [0u8; 8];
        head[4..].copy_from_slice(&(2u32 << 20).to_le_bytes());
        assert!(header(head).is_err());
    }

    #[test]
    fn commands_carry_their_nonce_and_optional_event() {
        let plain = command("GET_GUILDS", &json!({}), None, "n1");
        assert_eq!(
            plain,
            json!({ "cmd": "GET_GUILDS", "args": {}, "nonce": "n1" })
        );
        let subscribe = command("SUBSCRIBE", &json!({}), Some("VOICE_CHANNEL_SELECT"), "n2");
        assert_eq!(subscribe["evt"], "VOICE_CHANNEL_SELECT");
    }

    #[test]
    fn sorts_replies_errors_and_events() {
        let reply =
            json!({ "cmd": "GET_GUILDS", "nonce": "n1", "data": { "guilds": [] }, "evt": null });
        assert_eq!(
            classify(&reply),
            Some(Incoming::Reply {
                nonce: "n1".into(),
                result: Ok(json!({ "guilds": [] }))
            })
        );
        let error = json!({ "cmd": "AUTHORIZE", "nonce": "n2", "evt": "ERROR", "data": { "code": 5000, "message": "OAuth2 Error: access_denied" } });
        assert_eq!(
            classify(&error),
            Some(Incoming::Reply {
                nonce: "n2".into(),
                result: Err(RpcError {
                    code: 5000,
                    message: "OAuth2 Error: access_denied".into()
                })
            })
        );
        let event = json!({ "cmd": "DISPATCH", "evt": "VOICE_CHANNEL_SELECT", "data": { "channel_id": "9" }, "nonce": null });
        assert_eq!(
            classify(&event),
            Some(Incoming::Event {
                evt: "VOICE_CHANNEL_SELECT".into(),
                data: json!({ "channel_id": "9" })
            })
        );
        assert_eq!(classify(&json!({ "cmd": "DISPATCH" })), None);
    }
}
