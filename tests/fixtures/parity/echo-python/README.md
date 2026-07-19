# echo-python — full event + context surface, runtime parity (Python)

The Python member of the echo parity set (`echo-bun`, `echo-python`,
`echo-rust`). Echoes the AWS event envelope and Lambda context back as JSON in
the same shape as the other two runtimes — `tests/runtime_parity_echo.rs`
asserts they match.

**Capability:** complete event surface + Lambda context, from a plain
`def lambda_handler(event, context)`. Honors `?status=NNN`.
**Runtime:** Python · **Handler:** `main.py` → `lambda_handler`

## Wiring (`examples/riz.all.toml`)

```toml
[function.echo-python]
runtime = "python"
handler = "./tests/fixtures/parity/echo-python/main.lambda_handler"
[[function.echo-python.routes]]
path = "/echo-python"
method = "ANY"
```

## Run

```bash
curl -X POST -H 'content-type: application/json' \
  -d '{"hello":"world"}' http://127.0.0.1:3000/echo-python
# {"echo":"/echo-python","method":"POST","functionName":"echo-python",...}
```

The handler uses only the Python standard library — no SDK, no `pip install`.
