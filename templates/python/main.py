# AWS API Gateway v2 HTTP Lambda handler.
# Runs on riz (https://riz.dev) via the embedded Python adapter.

import json


def lambda_handler(event, context):
    qs = event.get("queryStringParameters") or {}
    name = qs.get("name", "world")
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps(
            {
                "message": f"hello, {name}",
                "method": (event.get("requestContext") or {})
                .get("http", {})
                .get("method"),
                "path": event.get("rawPath"),
                "functionName": context.function_name,
                "awsRequestId": context.aws_request_id,
                "remainingMs": context.get_remaining_time_in_millis(),
            }
        ),
    }
