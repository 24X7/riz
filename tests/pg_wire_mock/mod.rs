//! Minimal in-process Postgres wire-protocol server — just enough protocol
//! for tokio-postgres over `sslmode=disable` to startup, prepare, bind,
//! execute, and run simple queries. Deterministic and dependency-free (no
//! Docker, no real PG) so broker wire tests run anywhere CI does.
//!
//! Behavior matrix (keyed on the SQL text):
//! - `... from orders ...`   → two columns (id int4, status text), one row
//!   `(1042, "delayed")` — proves typed column → JSON mapping.
//! - `select $1::text ...`   → echoes the bound param back — proves binding.
//! - `... pg_sleep ...`      → never answers the Execute — proves the
//!   broker's per-call deadline bounds a stalled backend.
//! - anything else          → zero rows.
//!
//! Every simple query (`SET …`, `BEGIN READ ONLY`, `COMMIT`) and every
//! `parse:`/`bind:` is appended to a shared log the test can assert on.

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub struct MockPgServer {
    pub port: u16,
    pub log: Arc<std::sync::Mutex<Vec<String>>>,
}

impl MockPgServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock pg");
        let port = listener.local_addr().unwrap().port();
        let log: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
        let log2 = log.clone();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                let log = log2.clone();
                tokio::spawn(async move {
                    let _ = handle_conn(sock, log).await;
                });
            }
        });
        Self { port, log }
    }

    pub fn dsn(&self) -> String {
        format!(
            "postgres://test@127.0.0.1:{}/test?sslmode=disable",
            self.port
        )
    }

    pub fn log_contains(&self, needle: &str) -> bool {
        self.log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains(needle))
    }
}

// ── wire helpers ──────────────────────────────────────────────────────────

fn msg(t: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 5);
    out.push(t);
    out.extend_from_slice(&((payload.len() as i32 + 4).to_be_bytes()));
    out.extend_from_slice(payload);
    out
}

fn cstr(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

fn ready_for_query(state: u8) -> Vec<u8> {
    msg(b'Z', &[state])
}

fn command_complete(tag: &str) -> Vec<u8> {
    msg(b'C', &cstr(tag))
}

/// RowDescription for (name, type_oid) columns. Text format.
fn row_description(cols: &[(&str, i32)]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&(cols.len() as i16).to_be_bytes());
    for (name, oid) in cols {
        p.extend_from_slice(&cstr(name));
        p.extend_from_slice(&0i32.to_be_bytes()); // table oid
        p.extend_from_slice(&0i16.to_be_bytes()); // attr num
        p.extend_from_slice(&oid.to_be_bytes()); // type oid
        p.extend_from_slice(&(-1i16).to_be_bytes()); // typlen
        p.extend_from_slice(&(-1i32).to_be_bytes()); // typmod
        p.extend_from_slice(&0i16.to_be_bytes()); // format = text
    }
    msg(b'T', &p)
}

/// DataRow with raw field encodings. tokio-postgres requests BINARY result
/// format in Bind, so int4 must be 4-byte big-endian; text's binary form is
/// its utf8 bytes.
fn data_row(values: &[&[u8]]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&(values.len() as i16).to_be_bytes());
    for v in values {
        p.extend_from_slice(&(v.len() as i32).to_be_bytes());
        p.extend_from_slice(v);
    }
    msg(b'D', &p)
}

const OID_INT4: i32 = 23;
const OID_TEXT: i32 = 25;

fn read_cstr(buf: &[u8], at: &mut usize) -> String {
    let start = *at;
    while buf[*at] != 0 {
        *at += 1;
    }
    let s = String::from_utf8_lossy(&buf[start..*at]).into_owned();
    *at += 1;
    s
}

// ── connection state machine ─────────────────────────────────────────────

async fn handle_conn(
    mut s: TcpStream,
    log: Arc<std::sync::Mutex<Vec<String>>>,
) -> std::io::Result<()> {
    // Startup message: i32 length, then payload (no type byte).
    let mut len4 = [0u8; 4];
    s.read_exact(&mut len4).await?;
    let len = i32::from_be_bytes(len4) as usize;
    let mut startup = vec![0u8; len - 4];
    s.read_exact(&mut startup).await?;

    // AuthenticationOk + BackendKeyData + ReadyForQuery.
    let mut hello = Vec::new();
    hello.extend_from_slice(&msg(b'R', &0i32.to_be_bytes()));
    let mut keydata = Vec::new();
    keydata.extend_from_slice(&1234i32.to_be_bytes());
    keydata.extend_from_slice(&5678i32.to_be_bytes());
    hello.extend_from_slice(&msg(b'K', &keydata));
    hello.extend_from_slice(&ready_for_query(b'I'));
    s.write_all(&hello).await?;

    let mut in_txn = false;
    let mut stmt_query = String::new();
    let mut stmt_param_oids: Vec<i32> = Vec::new();
    let mut bound_params: Vec<String> = Vec::new();

    loop {
        let mut t = [0u8; 1];
        if s.read_exact(&mut t).await.is_err() {
            return Ok(()); // client hung up
        }
        s.read_exact(&mut len4).await?;
        let len = i32::from_be_bytes(len4) as usize;
        let mut payload = vec![0u8; len - 4];
        s.read_exact(&mut payload).await?;

        match t[0] {
            // Simple query — SET / BEGIN READ ONLY / COMMIT.
            b'Q' => {
                let mut at = 0;
                let q = read_cstr(&payload, &mut at);
                log.lock().unwrap().push(format!("query: {q}"));
                let upper = q.to_uppercase();
                let (tag, state) = if upper.starts_with("BEGIN")
                    || upper.starts_with("START TRANSACTION")
                {
                    in_txn = true;
                    ("BEGIN", b'T')
                } else if upper.starts_with("COMMIT") {
                    in_txn = false;
                    ("COMMIT", b'I')
                } else if upper.starts_with("SET") {
                    ("SET", if in_txn { b'T' } else { b'I' })
                } else {
                    ("SELECT 0", if in_txn { b'T' } else { b'I' })
                };
                let mut out = command_complete(tag);
                out.extend_from_slice(&ready_for_query(state));
                s.write_all(&out).await?;
            }
            // Parse: stmt name, query, param-type oids.
            b'P' => {
                let mut at = 0;
                let _name = read_cstr(&payload, &mut at);
                stmt_query = read_cstr(&payload, &mut at);
                let n = i16::from_be_bytes([payload[at], payload[at + 1]]) as usize;
                at += 2;
                stmt_param_oids = (0..n)
                    .map(|k| {
                        i32::from_be_bytes([
                            payload[at + k * 4],
                            payload[at + k * 4 + 1],
                            payload[at + k * 4 + 2],
                            payload[at + k * 4 + 3],
                        ])
                    })
                    .collect();
                log.lock().unwrap().push(format!("parse: {stmt_query}"));
                s.write_all(&msg(b'1', &[])).await?; // ParseComplete
            }
            // Describe (statement): ParameterDescription + RowDescription.
            b'D' => {
                let mut out = Vec::new();
                let mut pd = Vec::new();
                pd.extend_from_slice(&(stmt_param_oids.len() as i16).to_be_bytes());
                for oid in &stmt_param_oids {
                    pd.extend_from_slice(&oid.to_be_bytes());
                }
                out.extend_from_slice(&msg(b't', &pd));
                out.extend_from_slice(&describe_rows(&stmt_query));
                s.write_all(&out).await?;
            }
            // Bind: record text params.
            b'B' => {
                let mut at = 0;
                let _portal = read_cstr(&payload, &mut at);
                let _stmt = read_cstr(&payload, &mut at);
                let nfmt = i16::from_be_bytes([payload[at], payload[at + 1]]) as usize;
                at += 2 + nfmt * 2;
                let nparams = i16::from_be_bytes([payload[at], payload[at + 1]]) as usize;
                at += 2;
                bound_params.clear();
                for _ in 0..nparams {
                    let plen = i32::from_be_bytes([
                        payload[at],
                        payload[at + 1],
                        payload[at + 2],
                        payload[at + 3],
                    ]);
                    at += 4;
                    if plen < 0 {
                        bound_params.push("NULL".into());
                    } else {
                        let val =
                            String::from_utf8_lossy(&payload[at..at + plen as usize]).into_owned();
                        at += plen as usize;
                        bound_params.push(val);
                    }
                }
                log.lock()
                    .unwrap()
                    .push(format!("bind: {:?}", bound_params));
                s.write_all(&msg(b'2', &[])).await?; // BindComplete
            }
            // Execute: emit rows per the behavior matrix. BEGIN/COMMIT may
            // arrive through the extended protocol too (tokio-postgres
            // transactions), not just as simple queries.
            b'E' => {
                if stmt_query.contains("pg_sleep") {
                    // Stall forever — the broker's deadline must cut this.
                    tokio::time::sleep(std::time::Duration::from_secs(600)).await;
                    return Ok(());
                }
                let upper = stmt_query.to_uppercase();
                let mut out = Vec::new();
                if upper.starts_with("BEGIN") {
                    in_txn = true;
                    out.extend_from_slice(&command_complete("BEGIN"));
                } else if upper.starts_with("COMMIT") {
                    in_txn = false;
                    out.extend_from_slice(&command_complete("COMMIT"));
                } else if stmt_query.contains("from orders") {
                    out.extend_from_slice(&data_row(&[&1042i32.to_be_bytes(), b"delayed"]));
                    out.extend_from_slice(&command_complete("SELECT 1"));
                } else if stmt_query.contains("$1::text") {
                    let echo = bound_params.first().cloned().unwrap_or_default();
                    out.extend_from_slice(&data_row(&[echo.as_bytes()]));
                    out.extend_from_slice(&command_complete("SELECT 1"));
                } else {
                    out.extend_from_slice(&command_complete("SELECT 0"));
                }
                s.write_all(&out).await?;
            }
            // Sync.
            b'S' => {
                s.write_all(&ready_for_query(if in_txn { b'T' } else { b'I' }))
                    .await?;
            }
            // Close (statement/portal drop) → CloseComplete.
            b'C' => {
                s.write_all(&msg(b'3', &[])).await?;
            }
            // Terminate.
            b'X' => return Ok(()),
            other => {
                log.lock()
                    .unwrap()
                    .push(format!("unhandled message type {}", other as char));
                return Ok(());
            }
        }
    }
}

fn describe_rows(query: &str) -> Vec<u8> {
    if query.contains("from orders") {
        row_description(&[("id", OID_INT4), ("status", OID_TEXT)])
    } else if query.contains("$1::text") {
        row_description(&[("echo", OID_TEXT)])
    } else if query.contains("pg_sleep") {
        row_description(&[("pg_sleep", OID_TEXT)])
    } else {
        msg(b'n', &[]) // NoData
    }
}
