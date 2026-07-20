//! BCP — the broker call protocol framing on the UDS between a `__wasm-host`
//! child and the daemon's broker service.
//!
//! Frame: `u32 len (LE) | u8 ver | u8 type | u16 flags | u64 call_id | payload`.
//! `len` counts the bytes AFTER itself (ver + type + flags + call_id +
//! payload). Frames are capped so a hostile or desynced peer can never make
//! the reader allocate unbounded memory (SAFETY rule 3).
//!
//! v1 is strict request/response — a sync wasip1 guest has at most one call in
//! flight — so `call_id` and `flags` are carried but unused; they leave room
//! for multiplexing and streaming without a wire break.

/// Wire version this build speaks.
pub const VERSION: u8 = 1;

/// Header bytes after the `len` prefix: ver(1) + type(1) + flags(2) + id(8).
const HEADER_TAIL: usize = 12;

/// Max payload a single frame may carry. Larger than any grant's payload caps
/// (which the dispatcher enforces per-call); this is the transport backstop.
pub const MAX_PAYLOAD: usize = 4 * 1024 * 1024;

/// Frame types. Unknown discriminants decode to [`FrameType::Unknown`] so a
/// desynced peer is a clean protocol error, never a panic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    /// child → daemon: `payload = 32-byte token`. Authorizes the connection.
    Hello,
    /// child → daemon: a capability call (see [`CallPayload`]).
    Call,
    /// daemon → child: the JSON response envelope for a call.
    Reply,
    /// Decoded from any byte this build does not recognize.
    Unknown(u8),
}

impl FrameType {
    fn to_byte(self) -> u8 {
        match self {
            FrameType::Hello => 1,
            FrameType::Call => 2,
            FrameType::Reply => 3,
            FrameType::Unknown(b) => b,
        }
    }
    fn from_byte(b: u8) -> Self {
        match b {
            1 => FrameType::Hello,
            2 => FrameType::Call,
            3 => FrameType::Reply,
            other => FrameType::Unknown(other),
        }
    }
}

/// A decoded frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub ver: u8,
    pub frame_type: FrameType,
    pub flags: u16,
    pub call_id: u64,
    pub payload: Vec<u8>,
}

impl Frame {
    /// Build a frame at the current wire version with no flags set.
    pub fn new(frame_type: FrameType, call_id: u64, payload: Vec<u8>) -> Self {
        Self {
            ver: VERSION,
            frame_type,
            flags: 0,
            call_id,
            payload,
        }
    }

    /// Serialize to the length-prefixed wire form. Returns an error if the
    /// payload exceeds [`MAX_PAYLOAD`] rather than emitting a frame no
    /// conforming reader would accept.
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        if self.payload.len() > MAX_PAYLOAD {
            return Err(WireError::TooLarge(self.payload.len()));
        }
        // payload.len() <= MAX_PAYLOAD (checked above), so neither add can
        // overflow usize on any supported target.
        let body_len = HEADER_TAIL.saturating_add(self.payload.len());
        let mut out = Vec::with_capacity(body_len.saturating_add(4));
        out.extend_from_slice(&(body_len as u32).to_le_bytes());
        out.push(self.ver);
        out.push(self.frame_type.to_byte());
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&self.call_id.to_le_bytes());
        out.extend_from_slice(&self.payload);
        Ok(out)
    }

    /// The length prefix declares `body_len`; the caller reads exactly that
    /// many bytes and hands them here. Rejects a short body or an oversize
    /// declared length before any allocation the peer controls.
    pub fn decode_body(body: &[u8]) -> Result<Frame, WireError> {
        let head = body.get(..HEADER_TAIL).ok_or(WireError::Truncated)?;
        // head is exactly HEADER_TAIL (12) bytes: ver, type, flags[2], id[8].
        let ver = *head.first().ok_or(WireError::Truncated)?;
        let frame_type = FrameType::from_byte(*head.get(1).ok_or(WireError::Truncated)?);
        let flags_bytes = head.get(2..4).ok_or(WireError::Truncated)?;
        let flags = u16::from_le_bytes([
            *flags_bytes.first().ok_or(WireError::Truncated)?,
            *flags_bytes.get(1).ok_or(WireError::Truncated)?,
        ]);
        let id_bytes: [u8; 8] = head
            .get(4..12)
            .ok_or(WireError::Truncated)?
            .try_into()
            .map_err(|_| WireError::Truncated)?;
        let call_id = u64::from_le_bytes(id_bytes);
        let payload = body.get(HEADER_TAIL..).ok_or(WireError::Truncated)?.to_vec();
        Ok(Frame {
            ver,
            frame_type,
            flags,
            call_id,
            payload,
        })
    }

    /// Parse a declared body length from a `len` prefix, rejecting anything
    /// that would exceed the frame cap before the caller reads the body.
    pub fn declared_body_len(prefix: [u8; 4]) -> Result<usize, WireError> {
        let n = u32::from_le_bytes(prefix) as usize;
        let payload_len = n.checked_sub(HEADER_TAIL).ok_or(WireError::Truncated)?;
        if payload_len > MAX_PAYLOAD {
            return Err(WireError::TooLarge(payload_len));
        }
        Ok(n)
    }
}

/// The CALL payload: `u16 verb_len | verb | u16 grant_len | grant | body`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallPayload {
    pub verb: String,
    pub grant: String,
    pub body: Vec<u8>,
}

impl CallPayload {
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        if self.verb.len() > u16::MAX as usize || self.grant.len() > u16::MAX as usize {
            return Err(WireError::TooLarge(self.verb.len().max(self.grant.len())));
        }
        let cap = 4usize
            .saturating_add(self.verb.len())
            .saturating_add(self.grant.len())
            .saturating_add(self.body.len());
        let mut out = Vec::with_capacity(cap);
        out.extend_from_slice(&(self.verb.len() as u16).to_le_bytes());
        out.extend_from_slice(self.verb.as_bytes());
        out.extend_from_slice(&(self.grant.len() as u16).to_le_bytes());
        out.extend_from_slice(self.grant.as_bytes());
        out.extend_from_slice(&self.body);
        Ok(out)
    }

    pub fn decode(payload: &[u8]) -> Result<CallPayload, WireError> {
        let mut cur = 0usize;
        let verb = read_len_prefixed_str(payload, &mut cur)?;
        let grant = read_len_prefixed_str(payload, &mut cur)?;
        let body = payload.get(cur..).ok_or(WireError::Truncated)?.to_vec();
        Ok(CallPayload { verb, grant, body })
    }
}

fn read_len_prefixed_str(buf: &[u8], cur: &mut usize) -> Result<String, WireError> {
    let lo = *cur;
    let hi = lo.checked_add(2).ok_or(WireError::Truncated)?;
    let len_bytes = buf.get(lo..hi).ok_or(WireError::Truncated)?;
    let len = u16::from_le_bytes([
        *len_bytes.first().ok_or(WireError::Truncated)?,
        *len_bytes.get(1).ok_or(WireError::Truncated)?,
    ]) as usize;
    let start = hi;
    let end = start.checked_add(len).ok_or(WireError::Truncated)?;
    let bytes = buf.get(start..end).ok_or(WireError::Truncated)?;
    *cur = end;
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

/// Framing-layer errors — all recoverable (the caller drops the connection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    /// Fewer bytes than the frame/field structure requires.
    Truncated,
    /// A declared length exceeds the transport cap.
    TooLarge(usize),
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::Truncated => write!(f, "frame truncated"),
            WireError::TooLarge(n) => write!(f, "frame payload {n} exceeds cap {MAX_PAYLOAD}"),
        }
    }
}

impl std::error::Error for WireError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(frame: &Frame) -> Frame {
        let bytes = frame.encode().expect("encode");
        let mut prefix = [0u8; 4];
        prefix.copy_from_slice(&bytes[..4]);
        let body_len = Frame::declared_body_len(prefix).expect("len");
        assert_eq!(body_len, bytes.len() - 4);
        Frame::decode_body(&bytes[4..]).expect("decode")
    }

    #[test]
    fn frame_roundtrips() {
        let f = Frame::new(FrameType::Call, 42, b"hello".to_vec());
        assert_eq!(roundtrip(&f), f);
    }

    #[test]
    fn hello_and_reply_roundtrip() {
        for ft in [FrameType::Hello, FrameType::Reply] {
            let f = Frame::new(ft, 0, vec![7u8; 32]);
            assert_eq!(roundtrip(&f), f);
        }
    }

    #[test]
    fn unknown_type_decodes_not_panics() {
        let mut bytes = Frame::new(FrameType::Call, 1, b"x".to_vec()).encode().unwrap();
        bytes[5] = 99; // frame_type byte
        let f = Frame::decode_body(&bytes[4..]).expect("decode");
        assert_eq!(f.frame_type, FrameType::Unknown(99));
    }

    #[test]
    fn truncated_body_is_rejected() {
        assert_eq!(Frame::decode_body(&[0u8; 5]), Err(WireError::Truncated));
    }

    #[test]
    fn oversize_declared_len_is_rejected() {
        let prefix = ((MAX_PAYLOAD + HEADER_TAIL + 1) as u32).to_le_bytes();
        assert!(matches!(
            Frame::declared_body_len(prefix),
            Err(WireError::TooLarge(_))
        ));
    }

    #[test]
    fn short_declared_len_is_rejected() {
        assert_eq!(Frame::declared_body_len(3u32.to_le_bytes()), Err(WireError::Truncated));
    }

    #[test]
    fn encode_rejects_oversize_payload() {
        let f = Frame::new(FrameType::Reply, 0, vec![0u8; MAX_PAYLOAD + 1]);
        assert!(matches!(f.encode(), Err(WireError::TooLarge(_))));
    }

    #[test]
    fn call_payload_roundtrips() {
        let c = CallPayload {
            verb: "pg.query".into(),
            grant: "db".into(),
            body: br#"{"sql":"select 1"}"#.to_vec(),
        };
        assert_eq!(CallPayload::decode(&c.encode().unwrap()).unwrap(), c);
    }

    #[test]
    fn call_payload_empty_body_ok() {
        let c = CallPayload {
            verb: "pg.query".into(),
            grant: "g".into(),
            body: Vec::new(),
        };
        assert_eq!(CallPayload::decode(&c.encode().unwrap()).unwrap(), c);
    }

    #[test]
    fn call_payload_truncated_is_rejected() {
        // Declares verb_len = 10 but supplies fewer bytes.
        let bad = [10u8, 0, b'p', b'g'];
        assert_eq!(CallPayload::decode(&bad), Err(WireError::Truncated));
    }
}
