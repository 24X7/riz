//! The child-side capability client — a synchronous UDS stub the
//! `__wasm-host` process uses to reach the daemon's broker.
//!
//! Deliberately sync `std::os::unix::net::UnixStream`, not tokio: a wasip1
//! guest is single-threaded and blocks inside the host call anyway, so the
//! child needs no async runtime at all (the per-child tokio runtime the v1
//! broker required is gone). Read/write timeouts are armed at
//! `call_timeout_ms + slack` (passed in `RIZ_BROKER_TIMEOUT_MS`) so a wedged
//! daemon connection returns a structured `timeout` envelope and reconnects —
//! never a hung guest.

use super::wire::{CallPayload, Frame, FrameType};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

pub struct CapabilityClient {
    sock_path: PathBuf,
    token: String,
    timeout: Duration,
    /// Lazily-established, HELLO-authenticated connection; dropped on any I/O
    /// error so the next call reconnects (supervised reconnect, no backoff
    /// needed — a wasip1 guest calls serially).
    conn: Option<UnixStream>,
    /// Monotonic call id, purely for frame tagging (v1 is serial).
    next_id: u64,
}

impl CapabilityClient {
    /// Build from the env a granted worker was spawned with. `None` for a
    /// grantless worker (no `RIZ_BROKER_SOCK`) — the host import then answers
    /// `denied` locally with zero IPC.
    pub fn from_env() -> Option<CapabilityClient> {
        let sock_path = std::env::var("RIZ_BROKER_SOCK").ok()?;
        let token = std::env::var("RIZ_BROKER_TOKEN").ok()?;
        let timeout_ms = std::env::var("RIZ_BROKER_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(2_000);
        Some(CapabilityClient {
            sock_path: PathBuf::from(sock_path),
            token,
            timeout: Duration::from_millis(timeout_ms),
            conn: None,
            next_id: 0,
        })
    }

    /// Forward one capability call to the daemon and return the response
    /// envelope bytes. ALWAYS returns bytes the guest can parse: an I/O or
    /// timeout failure becomes a `timeout`/`backend` envelope, and the
    /// connection is dropped so the next call reconnects.
    pub fn call(&mut self, verb: &str, grant: &str, body: &[u8]) -> Vec<u8> {
        match self.try_call(verb, grant, body) {
            Ok(bytes) => bytes,
            Err(e) => {
                // A broken connection must not wedge the guest: drop it, and
                // hand back a structured envelope from the closed error set.
                self.conn = None;
                envelope("timeout", &format!("broker call failed: {e}"))
            }
        }
    }

    fn try_call(&mut self, verb: &str, grant: &str, body: &[u8]) -> std::io::Result<Vec<u8>> {
        self.ensure_connected()?;
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);

        let payload = CallPayload {
            verb: verb.to_string(),
            grant: grant.to_string(),
            body: body.to_vec(),
        }
        .encode()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        let frame = Frame::new(FrameType::Call, id, payload)
            .encode()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

        let conn = self
            .conn
            .as_mut()
            .ok_or_else(|| std::io::Error::other("no connection"))?;
        conn.write_all(&frame)?;
        conn.flush()?;
        let reply = read_frame(conn)?;
        if reply.frame_type != FrameType::Reply {
            return Err(std::io::Error::other("expected REPLY frame"));
        }
        // v1 is strictly serial, so the reply id must match the call id; a
        // mismatch means the stream desynced — treat it as an I/O error so the
        // connection is dropped and rebuilt.
        if reply.call_id != id {
            return Err(std::io::Error::other("reply id mismatch (desync)"));
        }
        Ok(reply.payload)
    }

    /// Connect + HELLO-authenticate if not already connected.
    fn ensure_connected(&mut self) -> std::io::Result<()> {
        if self.conn.is_some() {
            return Ok(());
        }
        let stream = UnixStream::connect(&self.sock_path)?;
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_write_timeout(Some(self.timeout))?;
        let mut stream = stream;
        let hello = Frame::new(FrameType::Hello, 0, self.token.as_bytes().to_vec())
            .encode()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        stream.write_all(&hello)?;
        stream.flush()?;
        self.conn = Some(stream);
        Ok(())
    }
}

/// Read one length-prefixed BCP frame from a blocking stream.
fn read_frame(stream: &mut UnixStream) -> std::io::Result<Frame> {
    let mut prefix = [0u8; 4];
    stream.read_exact(&mut prefix)?;
    let body_len = Frame::declared_body_len(prefix)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut body = vec![0u8; body_len];
    stream.read_exact(&mut body)?;
    Frame::decode_body(&body).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// A closed-error-set envelope the guest can parse.
fn envelope(code: &str, message: &str) -> Vec<u8> {
    serde_json::json!({ "ok": false, "error": { "code": code, "message": message } })
        .to_string()
        .into_bytes()
}
