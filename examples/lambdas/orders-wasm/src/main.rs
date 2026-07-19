//! orders-wasm — the real-compute member of the WASM example set.
//!
//! Authored as a pure Lambda handler on `riz-wasm`: the shim owns the wire;
//! this file owns the business logic. Compiled to `wasm32-wasip1` and run by
//! `riz __wasm-host` inside wasmtime's WASI capability sandbox (deny-by-default
//! fs/net), it parses an order payload (an array of line-items), validates
//! every field, and prices the order — per-line extended amounts, subtotal,
//! tax, and grand total — returning a structured JSON Lambda response.
//!
//! It proves a `.wasm` handler is a first-class riz runtime that can run actual
//! application logic, not just bounce the event back. Pure sync std +
//! serde_json — no tokio, no networking — fully deterministic across hosts.

use riz_wasm::{Context, Error, Event, Response};

/// Tax rate applied to the order subtotal, in basis points (825 = 8.25%).
/// Fixed + integer math keeps the compute fully deterministic across hosts.
const TAX_RATE_BPS: i64 = 825;

fn main() {
    riz_wasm::run(handler)
}

fn handler(event: Event, _ctx: Context) -> Result<Response, Error> {
    // The order payload arrives as the request body (a JSON string in the AWS
    // proxy event). Parse it out, then compute.
    let body_str = event.raw().get("body").and_then(|b| b.as_str()).unwrap_or("");
    let body: serde_json::Value = match serde_json::from_str(body_str) {
        Ok(v) => v,
        Err(_) => return Ok(response(400, &error_body("request body is not valid json"))),
    };

    Ok(match price_order(&body) {
        Ok(result) => response(200, &result),
        Err(msg) => response(422, &error_body(&msg)),
    })
}

/// The core compute: validate + price an order.
///
/// Expected body shape:
///   { "currency": "USD", "items": [ { "sku": "A", "qty": 2, "unitPriceCents": 500 }, ... ] }
///
/// All money is integer cents — no floats — so the totals are byte-identical on
/// every host. Returns the structured summary on success, or a validation
/// message (mapped to HTTP 422) on the first invalid field.
fn price_order(body: &serde_json::Value) -> Result<serde_json::Value, String> {
    let currency = body
        .get("currency")
        .and_then(|c| c.as_str())
        .unwrap_or("USD");
    if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_uppercase()) {
        return Err(format!("currency must be a 3-letter ISO code, got {currency:?}"));
    }

    let items = body
        .get("items")
        .and_then(|i| i.as_array())
        .ok_or_else(|| "items must be an array".to_string())?;
    if items.is_empty() {
        return Err("order must contain at least one line item".to_string());
    }

    let mut subtotal_cents: i64 = 0;
    let mut total_qty: i64 = 0;
    let mut priced_lines = Vec::with_capacity(items.len());

    for (idx, item) in items.iter().enumerate() {
        let sku = item
            .get("sku")
            .and_then(|s| s.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("item[{idx}]: sku is required and must be a non-empty string"))?;

        let qty = item
            .get("qty")
            .and_then(|q| q.as_i64())
            .ok_or_else(|| format!("item[{idx}] ({sku}): qty must be an integer"))?;
        if qty <= 0 {
            return Err(format!("item[{idx}] ({sku}): qty must be positive, got {qty}"));
        }

        let unit_price_cents = item
            .get("unitPriceCents")
            .and_then(|p| p.as_i64())
            .ok_or_else(|| format!("item[{idx}] ({sku}): unitPriceCents must be an integer"))?;
        if unit_price_cents < 0 {
            return Err(format!(
                "item[{idx}] ({sku}): unitPriceCents must not be negative, got {unit_price_cents}"
            ));
        }

        let extended_cents = qty
            .checked_mul(unit_price_cents)
            .ok_or_else(|| format!("item[{idx}] ({sku}): line amount overflow"))?;
        subtotal_cents = subtotal_cents
            .checked_add(extended_cents)
            .ok_or_else(|| "order subtotal overflow".to_string())?;
        total_qty += qty;

        priced_lines.push(serde_json::json!({
            "sku": sku,
            "qty": qty,
            "unitPriceCents": unit_price_cents,
            "extendedCents": extended_cents,
        }));
    }

    // Tax = subtotal × rate, rounded half-up, all in integer cents.
    let tax_cents = (subtotal_cents * TAX_RATE_BPS + 5_000) / 10_000;
    let total_cents = subtotal_cents + tax_cents;

    Ok(serde_json::json!({
        "currency": currency,
        "lineItemCount": priced_lines.len(),
        "totalQuantity": total_qty,
        "lines": priced_lines,
        "subtotalCents": subtotal_cents,
        "taxRateBps": TAX_RATE_BPS,
        "taxCents": tax_cents,
        "totalCents": total_cents,
    }))
}

fn error_body(message: &str) -> serde_json::Value {
    serde_json::json!({ "error": message })
}

/// Wrap a JSON value in a canonical AWS Lambda proxy response.
fn response(status: i64, body: &serde_json::Value) -> Response {
    Response::from(serde_json::json!({
        "statusCode": status,
        "headers": { "content-type": "application/json", "x-riz-runtime": "wasm" },
        "multiValueHeaders": {},
        "body": body.to_string(),
        "isBase64Encoded": false,
        "cookies": [],
    }))
}
