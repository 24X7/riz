#!/usr/bin/env python3
# Bridges AWS Lambda HTTP API Gateway v2 handler → riz stdin/stdout protocol.
# Spawned by riz as: python3 python-adapter.py <handler_path.attribute>
#
# Wire format on stdin:  one JSON-encoded envelope per line:
#   { "event": <ApiGatewayV2httpRequest>, "__riz_deadline_ms": <epoch_ms>, "__riz_function_name": "<name>" }
# Falls back to bare event JSON (no envelope) for manual/direct invocations.
#
# Wire format on stdout: one JSON-encoded `ApiGatewayV2httpResponse` per line
#                        (statusCode, headers, multiValueHeaders, body, isBase64Encoded, cookies).
#
# The handler signature matches real AWS Lambda:
#   def lambda_handler(event, context): return { "statusCode": 200, ... }

import sys
import json
import os
import time
import uuid
import importlib
import importlib.util


def load_handler(handler_arg: str):
    """
    Resolve handler_arg (e.g. "/abs/path/to/app.lambda_handler" or "mypackage.app.lambda_handler")
    into (module, callable).

    The last dot separates the attribute name from the module path.
    If the module path contains a "/" it is a file path (sans .py extension).
    Otherwise it is a Python dotted module name.
    """
    module_path, attr = handler_arg.rsplit(".", 1)

    if "/" in module_path:
        # File path style — load by absolute or relative file path
        spec = importlib.util.spec_from_file_location("user_handler", module_path + ".py")
        if spec is None or spec.loader is None:
            raise ImportError(f"Cannot load module from file: {module_path}.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)  # type: ignore[union-attr]
    else:
        module = importlib.import_module(module_path)

    handler = getattr(module, attr)
    if not callable(handler):
        raise TypeError(f"Handler attribute '{attr}' in '{module_path}' is not callable")
    return handler


def make_context(function_name: str, arn: str, request_id: str, deadline_ms: int):
    class Context:
        def __init__(self) -> None:
            self.function_name: str = function_name
            self.invoked_function_arn: str = arn
            self.aws_request_id: str = request_id
            self.function_version: str = "$LATEST"
            self.memory_limit_in_mb: str = os.environ.get(
                "AWS_LAMBDA_FUNCTION_MEMORY_SIZE", "512"
            )
            self.log_group_name: str = os.environ.get("AWS_LAMBDA_LOG_GROUP_NAME", "/riz")
            self.log_stream_name: str = os.environ.get("AWS_LAMBDA_LOG_STREAM_NAME", "local")
            self._deadline_ms: int = deadline_ms

        def get_remaining_time_in_millis(self) -> int:
            return max(0, self._deadline_ms - int(time.time() * 1000))

    return Context()


def normalize_response(r) -> dict:
    """Normalize a handler return value to the canonical AWS API GW v2 shape."""
    if not isinstance(r, dict):
        r = {"statusCode": 200}
    status = r.get("statusCode")
    status = status if isinstance(status, int) else 200
    headers = r.get("headers")
    headers = headers if isinstance(headers, dict) else {}
    multi = r.get("multiValueHeaders")
    multi = multi if isinstance(multi, dict) else {}
    body = r.get("body")
    if body is None:
        body = ""
    elif not isinstance(body, str):
        body = str(body)
    b64 = r.get("isBase64Encoded") is True
    cookies = r.get("cookies")
    cookies = cookies if isinstance(cookies, list) else []
    return {
        "statusCode": status,
        "headers": headers,
        "multiValueHeaders": multi,
        "body": body,
        "isBase64Encoded": b64,
        "cookies": cookies,
    }


def main() -> None:
    if len(sys.argv) < 2:
        sys.stderr.write("riz python-adapter: missing handler argument\n")
        sys.stderr.flush()
        sys.exit(1)

    handler_arg = sys.argv[1]

    try:
        handler = load_handler(handler_arg)
    except Exception as e:
        sys.stderr.write(f"riz python-adapter: failed to load handler '{handler_arg}': {e}\n")
        sys.stderr.flush()
        sys.exit(1)

    # Redirect print() to stderr so it doesn't corrupt the stdout protocol stream.
    sys.stdout = sys.__stdout__

    for raw_line in sys.stdin:
        line = raw_line.strip()
        if not line:
            continue

        try:
            parsed = json.loads(line)
        except json.JSONDecodeError as e:
            err = {
                "statusCode": 400,
                "headers": {"content-type": "application/json"},
                "body": json.dumps({"message": f"bad event json: {e}"}),
                "isBase64Encoded": False,
                "cookies": [],
                "multiValueHeaders": {},
            }
            sys.stdout.write(json.dumps(err) + "\n")
            sys.stdout.flush()
            continue

        # Envelope: { event, __riz_deadline_ms, __riz_function_name }
        # Fall back to bare event if envelope keys missing (manual invocations).
        event = parsed.get("event", parsed)
        deadline_ms: int = parsed.get(
            "__riz_deadline_ms", int(time.time() * 1000) + 30_000
        )
        function_name: str = (
            parsed.get("__riz_function_name")
            or os.environ.get("AWS_LAMBDA_FUNCTION_NAME")
            or "unknown"
        )
        arn: str = os.environ.get(
            "AWS_LAMBDA_FUNCTION_ARN",
            f"arn:riz:lambda:local:000000000000:function:{function_name}",
        )
        rc = event.get("requestContext") if isinstance(event, dict) else None
        request_id: str = (
            (rc.get("requestId") if isinstance(rc, dict) else None)
            or str(uuid.uuid4())
        )

        context = make_context(function_name, arn, request_id, deadline_ms)

        try:
            result = handler(event, context)
            # Discriminate HTTP-response shape from raw payloads. HTTP-shape
            # returns (have numeric statusCode) are normalized so the Rust side
            # deserializes cleanly into ApiGatewayV2httpResponse. Raw payloads
            # (REQUEST authorizer responses like {isAuthorized, context}, future
            # non-HTTP event-source responses) pass through verbatim so the
            # caller (e.g. RequestAuthorizer via ProcessManager::invoke_generic)
            # sees the handler's actual return. BUG-20 was: every return value
            # was forced through HTTP normalization, silently dropping
            # {isAuthorized, context} authorizer payloads.
            if result is None:
                sys.stdout.write(json.dumps({
                    "statusCode": 204,
                    "headers": {},
                    "multiValueHeaders": {},
                    "body": "",
                    "isBase64Encoded": False,
                    "cookies": [],
                }) + "\n")
            elif isinstance(result, dict) and isinstance(result.get("statusCode"), int):
                safe_result = normalize_response(result)
                sys.stdout.write(json.dumps(safe_result) + "\n")
            else:
                sys.stdout.write(json.dumps(result) + "\n")
            sys.stdout.flush()
        except Exception as e:
            err = {
                "statusCode": 500,
                "headers": {"content-type": "application/json"},
                "body": json.dumps({"message": str(e)}),
                "isBase64Encoded": False,
                "cookies": [],
                "multiValueHeaders": {},
            }
            sys.stdout.write(json.dumps(err) + "\n")
            sys.stdout.flush()


if __name__ == "__main__":
    main()
