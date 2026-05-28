import json


def lambda_handler(event, context):
    # Honor ?status=NNN for the parity-H error-status test.
    qs = event.get("queryStringParameters") or {}
    try:
        status_code = int(qs.get("status", 200))
    except (TypeError, ValueError):
        status_code = 200
    return {
        "statusCode": status_code,
        "headers": {"content-type": "application/json", "x-riz-echo": "ok"},
        "cookies": ["sid=abc; Path=/"],
        "body": json.dumps({
            "echo": event.get("rawPath", ""),
            "method": (event.get("requestContext") or {}).get("http", {}).get("method"),
            "functionName": context.function_name,
            "invokedFunctionArn": context.invoked_function_arn,
            "awsRequestId": context.aws_request_id,
            "remainingMs": context.get_remaining_time_in_millis(),
            "body": event.get("body"),
            "isBase64Encoded": event.get("isBase64Encoded", False),
            "pathParameters": event.get("pathParameters"),
            "queryStringParameters": event.get("queryStringParameters"),
            "stageVariables": event.get("stageVariables"),
            "cookies": event.get("cookies"),
            "requestHeaders": event.get("headers"),
        }),
    }
