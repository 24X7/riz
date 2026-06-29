//! Postgres-wire backend for the broker's `pg_query` verb.
//!
//! One backend covers Neon, Supabase, RDS, and any self-hosted PG — they all
//! speak Postgres wire; only the DSN differs. TLS rides rustls + webpki
//! roots (no system TLS dependency, preserving the single-static-binary
//! story); `sslmode=disable` DSNs use plain TCP (local PG, CI mocks).
//!
//! Production posture:
//! - **Lazy connect, self-healing**: the connection is established on first
//!   use and dropped on any connection-level error so the next call
//!   reconnects — a flapping backend degrades to per-call errors, never a
//!   wedged worker.
//! - **Server-side statement timeout** (`[resources.pg.x] statement_timeout_ms`)
//!   is applied on connect — a second line of defense behind the broker's
//!   own per-call deadline.
//! - **read-only grants** run every query inside a `READ ONLY` transaction:
//!   writes are refused by Postgres itself, not by SQL inspection.
//! - **Params are text-typed** (`$1::int` style casts in SQL when a typed
//!   param is needed) — the lowest-common-denominator binding that works for
//!   every scalar without a type-inference matrix on the guest side.

use super::{PgBackend, PgRows};
use crate::config::PgResourceConfig;
use std::sync::Arc;
use tokio_postgres::types::Type;

pub struct TokioPgBackend {
    dsn: String,
    statement_timeout_ms: u64,
    /// Lazily-established client; dropped on connection errors so the next
    /// call reconnects.
    client: tokio::sync::Mutex<Option<tokio_postgres::Client>>,
}

impl TokioPgBackend {
    /// Build from a resource config; resolves the DSN from `dsn_env` NOW so
    /// a missing env var is a startup error, not a first-request surprise.
    pub fn from_resource(res: &PgResourceConfig) -> Result<Self, String> {
        let dsn = std::env::var(&res.dsn_env).map_err(|_| {
            format!(
                "resource dsn_env '{}' is not set in the host environment",
                res.dsn_env
            )
        })?;
        Ok(Self {
            dsn,
            statement_timeout_ms: res.statement_timeout_ms,
            client: tokio::sync::Mutex::new(None),
        })
    }

    async fn connect(&self) -> Result<tokio_postgres::Client, String> {
        let pg_config: tokio_postgres::Config =
            self.dsn.parse().map_err(|e| format!("invalid DSN: {e}"))?;
        let use_tls = !matches!(
            pg_config.get_ssl_mode(),
            tokio_postgres::config::SslMode::Disable
        );

        let client = if use_tls {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let tls_config = rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            let tls = tokio_postgres_rustls::MakeRustlsConnect::new(tls_config);
            let (client, connection) = pg_config
                .connect(tls)
                .await
                .map_err(|e| format!("pg connect (tls) failed: {e}"))?;
            // The connection future drives the socket; it ends when the
            // client drops or the backend hangs up.
            tokio::spawn(async move {
                if let Err(e) = connection.await {
                    tracing::warn!(target: "riz::broker", "pg connection ended: {e}");
                }
            });
            client
        } else {
            let (client, connection) = pg_config
                .connect(tokio_postgres::NoTls)
                .await
                .map_err(|e| format!("pg connect failed: {e}"))?;
            tokio::spawn(async move {
                if let Err(e) = connection.await {
                    tracing::warn!(target: "riz::broker", "pg connection ended: {e}");
                }
            });
            client
        };

        // Server-side statement timeout — defense in depth behind the
        // broker's per-call deadline.
        client
            .batch_execute(&format!(
                "SET statement_timeout = {}",
                self.statement_timeout_ms
            ))
            .await
            .map_err(|e| format!("failed to set statement_timeout: {e}"))?;
        Ok(client)
    }
}

#[async_trait::async_trait]
impl PgBackend for TokioPgBackend {
    async fn query(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        read_only: bool,
    ) -> Result<PgRows, String> {
        let mut guard = self.client.lock().await;
        if guard.is_none() {
            *guard = Some(self.connect().await?);
        }
        let client = guard.as_mut().expect("just ensured");

        let result = run_query(client, sql, params, read_only).await;
        match result {
            Ok(rows) => Ok(rows),
            Err(e) => {
                // Connection-level failure → drop the client so the next
                // call reconnects. Query-level errors keep the connection.
                if client.is_closed() {
                    *guard = None;
                }
                Err(e)
            }
        }
    }
}

async fn run_query(
    client: &mut tokio_postgres::Client,
    sql: &str,
    params: &[serde_json::Value],
    read_only: bool,
) -> Result<PgRows, String> {
    // Text-typed params: every placeholder is TEXT; SQL casts ($1::int)
    // pick the real type. JSON null → SQL NULL.
    let text_params: Vec<Option<String>> = params
        .iter()
        .map(|p| match p {
            serde_json::Value::Null => None,
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            serde_json::Value::Bool(b) => Some(b.to_string()),
            other => Some(other.to_string()), // arrays/objects as JSON text
        })
        .collect();
    let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = text_params
        .iter()
        .map(|p| p as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect();
    let param_types: Vec<Type> = params.iter().map(|_| Type::TEXT).collect();

    let rows = if read_only {
        // Writes are refused by Postgres itself, not by SQL inspection.
        let tx = client
            .build_transaction()
            .read_only(true)
            .start()
            .await
            .map_err(|e| format!("begin read-only failed: {e}"))?;
        let stmt = tx
            .prepare_typed(sql, &param_types)
            .await
            .map_err(|e| format!("prepare failed: {e}"))?;
        let rows = tx
            .query(&stmt, &param_refs)
            .await
            .map_err(|e| format!("query failed: {e}"))?;
        tx.commit()
            .await
            .map_err(|e| format!("commit failed: {e}"))?;
        rows
    } else {
        let stmt = client
            .prepare_typed(sql, &param_types)
            .await
            .map_err(|e| format!("prepare failed: {e}"))?;
        client
            .query(&stmt, &param_refs)
            .await
            .map_err(|e| format!("query failed: {e}"))?
    };

    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let mut obj = serde_json::Map::new();
        for (i, col) in row.columns().iter().enumerate() {
            obj.insert(col.name().to_string(), column_to_json(row, i, col.type_())?);
        }
        out.push(serde_json::Value::Object(obj));
    }
    Ok(PgRows { rows: out })
}

/// Map one column value to JSON. Covers the common scalar matrix; anything
/// else gets an actionable error naming the column and type (cast it with
/// `::text` in SQL).
fn column_to_json(
    row: &tokio_postgres::Row,
    i: usize,
    ty: &Type,
) -> Result<serde_json::Value, String> {
    use serde_json::Value;
    let v = match *ty {
        Type::BOOL => row
            .try_get::<_, Option<bool>>(i)
            .map(|v| v.map(Value::from)),
        Type::INT2 => row.try_get::<_, Option<i16>>(i).map(|v| v.map(Value::from)),
        Type::INT4 => row.try_get::<_, Option<i32>>(i).map(|v| v.map(Value::from)),
        Type::INT8 => row.try_get::<_, Option<i64>>(i).map(|v| v.map(Value::from)),
        Type::FLOAT4 => row.try_get::<_, Option<f32>>(i).map(|v| v.map(Value::from)),
        Type::FLOAT8 => row.try_get::<_, Option<f64>>(i).map(|v| v.map(Value::from)),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::UNKNOWN => row
            .try_get::<_, Option<String>>(i)
            .map(|v| v.map(Value::from)),
        Type::JSON | Type::JSONB => row
            .try_get::<_, Option<serde_json::Value>>(i)
            .map(|v| v.unwrap_or(Value::Null).into()),
        Type::UUID => row
            .try_get::<_, Option<uuid::Uuid>>(i)
            .map(|v| v.map(|u| Value::from(u.to_string()))),
        Type::TIMESTAMPTZ => row
            .try_get::<_, Option<chrono::DateTime<chrono::Utc>>>(i)
            .map(|v| v.map(|t| Value::from(t.to_rfc3339()))),
        Type::TIMESTAMP => row
            .try_get::<_, Option<chrono::NaiveDateTime>>(i)
            .map(|v| v.map(|t| Value::from(t.to_string()))),
        Type::DATE => row
            .try_get::<_, Option<chrono::NaiveDate>>(i)
            .map(|v| v.map(|d| Value::from(d.to_string()))),
        Type::BYTEA => row.try_get::<_, Option<Vec<u8>>>(i).map(|v| {
            v.map(|b| {
                use base64::Engine as _;
                Value::from(base64::engine::general_purpose::STANDARD.encode(b))
            })
        }),
        ref other => {
            return Err(format!(
                "column '{}' has unsupported type '{}' — cast it in SQL (e.g. \
                 \"{}::text\") to broker it",
                row.columns()[i].name(),
                other.name(),
                row.columns()[i].name(),
            ))
        }
    };
    v.map(|opt| opt.unwrap_or(Value::Null))
        .map_err(|e| format!("decode column '{}': {e}", row.columns()[i].name()))
}

/// Build the grant-name → backend map for one function from validated
/// config. Resolves every `[resources.pg.*]` referenced by a grant;
/// credentials (DSNs) are read host-side here and never serialized onward.
pub fn backends_for_function(
    grants: &indexmap::IndexMap<String, crate::config::CapabilityGrant>,
    resources: &crate::config::ResourcesConfig,
) -> Result<std::collections::HashMap<String, Arc<dyn PgBackend>>, String> {
    let mut map: std::collections::HashMap<String, Arc<dyn PgBackend>> =
        std::collections::HashMap::new();
    for (gname, grant) in grants {
        let Some((_, rname)) = grant.resource.split_once('.') else {
            return Err(format!("grant '{gname}': malformed resource"));
        };
        let res = resources
            .pg
            .get(rname)
            .ok_or_else(|| format!("grant '{gname}': unknown resource '{}'", grant.resource))?;
        map.insert(
            gname.clone(),
            Arc::new(TokioPgBackend::from_resource(res)?) as Arc<dyn PgBackend>,
        );
    }
    Ok(map)
}
