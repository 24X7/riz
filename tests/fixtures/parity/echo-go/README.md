# echo-go

The Go leg of riz's cross-runtime parity matrix — a **stock AWS Lambda Go
function** using the official `github.com/aws/aws-lambda-go` SDK
(`lambda.Start`). There is **no riz library**: this exact binary runs unmodified
on AWS Lambda and on riz, because riz implements the AWS Lambda Runtime API.

## Build & run

```bash
go build -o echo-go .
```

```toml
# riz.toml
[function.echo-go]
runtime = "go"                 # exec the native binary; riz serves the Runtime API
handler = "./echo-go"

[[function.echo-go.routes]]
path = "/echo"
method = "GET"
```

```bash
riz run
curl 'http://localhost:3000/echo?name=alice'
```
