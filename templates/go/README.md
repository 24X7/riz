# go riz template

A stock **AWS Lambda Go** function (`github.com/aws/aws-lambda-go`,
`lambda.Start`) — no riz library. The same binary runs unmodified on AWS Lambda
and on riz, because riz implements the AWS Lambda Runtime API.

```bash
go build -o hello .
riz run
curl 'http://localhost:3000/hello?name=alice'
# → {"message":"hello, alice","method":"GET","path":"/hello",...}
```

`runtime = "go"` in `riz.toml` tells riz to exec the native binary; riz sets
`AWS_LAMBDA_RUNTIME_API` so the official SDK connects to it.

**WebSocket variant:** WS handlers ($connect/$disconnect/$default +
@connections push) live as a showcase in `examples/chat`; scaffold any repo
subdir with `riz new <owner>/<repo>/<subdir>`.
