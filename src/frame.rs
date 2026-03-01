use serde_json::Value;

/// Outbound message on the per-connection write channel.
///
/// `Frame` variants are encoded lazily in the write task (control messages).
/// `Bytes` variants carry pre-encoded wire bytes (broadcast DELTAs encoded once,
/// shared across all subscribers to avoid N redundant msgpack encodes).
pub enum OutboundMsg {
    Frame(Frame),
    Bytes(Vec<u8>),
}

/// Build raw wire bytes for a DELTA frame, reusing a pre-encoded `body_mp`.
///
/// `body_mp` must be `rmp_serde::to_vec(&body_value)`.
/// Produces bytes identical to `Frame { frame_type: DELTA, stream_id, seq, body }.encode()`.
pub fn delta_bytes(stream_id: u32, seq: u64, body_mp: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(12 + body_mp.len());
    buf.push(0x94);         // fixarray(4)
    buf.push(types::DELTA); // 0x04, positive fixint
    write_uint(&mut buf, stream_id as u64);
    write_uint(&mut buf, seq);
    buf.extend_from_slice(body_mp);
    buf
}

/// Minimal MessagePack unsigned-integer encoder (most compact representation).
fn write_uint(buf: &mut Vec<u8>, v: u64) {
    match v {
        0x00..=0x7f => buf.push(v as u8),
        0x80..=0xff => { buf.push(0xcc); buf.push(v as u8); }
        0x100..=0xffff => {
            buf.push(0xcd);
            buf.push((v >> 8) as u8);
            buf.push(v as u8);
        }
        0x10000..=0xffff_ffff => {
            buf.push(0xce);
            buf.push((v >> 24) as u8); buf.push((v >> 16) as u8);
            buf.push((v >> 8) as u8);  buf.push(v as u8);
        }
        _ => {
            buf.push(0xcf);
            for i in (0..8).rev() { buf.push((v >> (i * 8)) as u8); }
        }
    }
}

/// Numeric frame type codes per SODP spec v0.2
pub mod types {
    pub const HELLO: u8 = 0x01;
    pub const WATCH: u8 = 0x02;
    pub const STATE_INIT: u8 = 0x03;
    pub const DELTA: u8 = 0x04;
    pub const CALL: u8 = 0x05;
    pub const RESULT: u8 = 0x06;
    pub const ERROR: u8 = 0x07;
    pub const ACK: u8 = 0x08;
    pub const HEARTBEAT: u8 = 0x09;
    pub const RESUME:    u8 = 0x0A;
    pub const AUTH:      u8 = 0x0B;
    pub const AUTH_OK:   u8 = 0x0C;
    pub const UNWATCH:   u8 = 0x0D;
}

/// A SODP wire frame.
///
/// Encoded as a 4-element MessagePack array: [frame_type, stream_id, seq, body]
///
/// - frame_type: u8  — one of the constants in `types`
/// - stream_id:  u32 — logical stream this frame belongs to (0 = control)
/// - seq:        u64 — monotonically increasing per stream
/// - body:       any — frame-type-specific payload
#[derive(Debug, Clone)]
pub struct Frame {
    pub frame_type: u8,
    pub stream_id: u32,
    pub seq: u64,
    pub body: Value,
}

impl Frame {
    pub fn encode(&self) -> Result<Vec<u8>, rmp_serde::encode::Error> {
        rmp_serde::to_vec(&(&self.frame_type, &self.stream_id, &self.seq, &self.body))
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, rmp_serde::decode::Error> {
        let (frame_type, stream_id, seq, body): (u8, u32, u64, Value) =
            rmp_serde::from_slice(bytes)?;
        Ok(Frame { frame_type, stream_id, seq, body })
    }
}

// --- Frame constructors ---

/// `auth`: whether the server requires a JWT AUTH handshake before accepting frames.
pub fn hello(auth: bool) -> Frame {
    Frame {
        frame_type: types::HELLO,
        stream_id:  0,
        seq:        0,
        body: serde_json::json!({ "version": "0.1", "server": "SODP-RS", "auth": auth }),
    }
}

/// AUTH_OK — sent after successful JWT validation.
/// Body echoes the `sub` claim so the client knows its identity.
pub fn auth_ok(sub: &str) -> Frame {
    Frame {
        frame_type: types::AUTH_OK,
        stream_id:  0,
        seq:        0,
        body: serde_json::json!({ "sub": sub }),
    }
}

pub fn state_init(stream_id: u32, seq: u64, state_key: &str, version: u64, value: Value, initialized: bool) -> Frame {
    Frame {
        frame_type: types::STATE_INIT,
        stream_id,
        seq,
        body: serde_json::json!({
            "state":       state_key,
            "version":     version,
            "value":       value,
            "initialized": initialized,
        }),
    }
}

pub fn delta(stream_id: u32, seq: u64, version: u64, ops: Value) -> Frame {
    Frame {
        frame_type: types::DELTA,
        stream_id,
        seq,
        body: serde_json::json!({ "version": version, "ops": ops }),
    }
}

pub fn result_ok(stream_id: u32, seq: u64, call_id: &str, data: Option<Value>) -> Frame {
    Frame {
        frame_type: types::RESULT,
        stream_id,
        seq,
        body: serde_json::json!({
            "call_id": call_id,
            "success": true,
            "data": data
        }),
    }
}

pub fn error(stream_id: u32, seq: u64, code: u32, message: &str) -> Frame {
    Frame {
        frame_type: types::ERROR,
        stream_id,
        seq,
        body: serde_json::json!({ "code": code, "message": message }),
    }
}

pub fn heartbeat() -> Frame {
    Frame {
        frame_type: types::HEARTBEAT,
        stream_id: 0,
        seq: 0,
        body: Value::Null,
    }
}
