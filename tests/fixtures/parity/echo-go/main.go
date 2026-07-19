// echo-go — the Go leg of riz's cross-runtime parity matrix.
//
// Written with the OFFICIAL AWS Lambda Go SDK (github.com/aws/aws-lambda-go) —
// there is NO riz library. This is a stock `lambda.Start(handler)` Lambda; the
// exact same binary runs on AWS Lambda and on riz, because riz implements the
// AWS Lambda Runtime API. Emits the canonical echo shape shared by
// echo-bun / echo-node / echo-python / echo-rust.
package main

import (
	"context"
	"encoding/json"
	"strconv"
	"strings"
	"time"

	"github.com/aws/aws-lambda-go/events"
	"github.com/aws/aws-lambda-go/lambda"
	"github.com/aws/aws-lambda-go/lambdacontext"
)

func handler(ctx context.Context, req events.APIGatewayV2HTTPRequest) (events.APIGatewayV2HTTPResponse, error) {
	awsRequestID, invokedArn := "", ""
	if lc, ok := lambdacontext.FromContext(ctx); ok {
		awsRequestID = lc.AwsRequestID
		invokedArn = lc.InvokedFunctionArn
	}

	// Remaining time from the context deadline — exactly as on AWS.
	var remainingMs int64
	if dl, ok := ctx.Deadline(); ok {
		remainingMs = time.Until(dl).Milliseconds()
		if remainingMs < 0 {
			remainingMs = 0
		}
	}

	reqHeaders := map[string]string{}
	for k, v := range req.Headers {
		reqHeaders[strings.ToLower(k)] = v
	}

	body := map[string]any{
		"echo":                  req.RawPath,
		"method":                req.RequestContext.HTTP.Method,
		"functionName":          lambdacontext.FunctionName,
		"invokedFunctionArn":    invokedArn,
		"awsRequestId":          awsRequestID,
		"remainingMs":           remainingMs,
		"body":                  bodyValue(req),
		"isBase64Encoded":       req.IsBase64Encoded,
		"pathParameters":        nonNilMap(req.PathParameters),
		"queryStringParameters": nonNilMap(req.QueryStringParameters),
		"stageVariables":        nonNilMap(req.StageVariables),
		"cookies":               req.Cookies,
		"requestHeaders":        reqHeaders,
	}
	b, _ := json.Marshal(body)

	// Honor ?status=NNN for the parity error-status test.
	status := 200
	if s := req.QueryStringParameters["status"]; s != "" {
		if n, err := strconv.Atoi(s); err == nil {
			status = n
		}
	}

	return events.APIGatewayV2HTTPResponse{
		StatusCode: status,
		Headers:    map[string]string{"content-type": "application/json", "x-riz-echo": "ok"},
		Body:       string(b),
		Cookies:    []string{"sid=abc; Path=/"},
	}, nil
}

func bodyValue(req events.APIGatewayV2HTTPRequest) any {
	if req.Body == "" {
		return nil
	}
	return req.Body
}

func nonNilMap(m map[string]string) map[string]string {
	if m == nil {
		return map[string]string{}
	}
	return m
}

func main() {
	lambda.Start(handler)
}
