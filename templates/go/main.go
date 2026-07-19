// AWS API Gateway v2 HTTP Lambda handler in Go, using the OFFICIAL AWS Lambda
// Go SDK (github.com/aws/aws-lambda-go). No riz library — this exact binary runs
// unmodified on AWS Lambda and on riz, because riz speaks the AWS Lambda Runtime
// API.
//
// Build: `go build -o hello .`, then point riz.toml's handler at `./hello`.
package main

import (
	"context"
	"encoding/json"
	"time"

	"github.com/aws/aws-lambda-go/events"
	"github.com/aws/aws-lambda-go/lambda"
	"github.com/aws/aws-lambda-go/lambdacontext"
)

func handler(ctx context.Context, req events.APIGatewayV2HTTPRequest) (events.APIGatewayV2HTTPResponse, error) {
	name := req.QueryStringParameters["name"]
	if name == "" {
		name = "world"
	}

	var remainingMs int64
	if dl, ok := ctx.Deadline(); ok {
		remainingMs = time.Until(dl).Milliseconds()
	}
	awsRequestID := ""
	if lc, ok := lambdacontext.FromContext(ctx); ok {
		awsRequestID = lc.AwsRequestID
	}

	body, _ := json.Marshal(map[string]any{
		"message":      "hello, " + name,
		"method":       req.RequestContext.HTTP.Method,
		"path":         req.RawPath,
		"functionName": lambdacontext.FunctionName,
		"awsRequestId": awsRequestID,
		"remainingMs":  remainingMs,
	})

	return events.APIGatewayV2HTTPResponse{
		StatusCode: 200,
		Headers:    map[string]string{"content-type": "application/json"},
		Body:       string(body),
	}, nil
}

func main() {
	lambda.Start(handler)
}
