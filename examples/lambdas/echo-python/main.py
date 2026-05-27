import json


def lambda_handler(event, context):
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps({
            "echo": event.get("rawPath", ""),
            "method": (event.get("requestContext") or {}).get("http", {}).get("method"),
            "functionName": context.function_name,
            "invokedFunctionArn": context.invoked_function_arn,
            "awsRequestId": context.aws_request_id,
            "remainingMs": context.get_remaining_time_in_millis(),
        }),
    }
