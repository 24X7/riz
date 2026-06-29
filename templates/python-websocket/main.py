# AWS API Gateway v2 WebSocket Lambda handler in Python.
# Runs on riz (https://riz.dev) via the embedded Python adapter.
#
# Three lifecycle events arrive at this single handler, distinguished
# by event["requestContext"]["routeKey"]:
#   $connect    — when a client opens the socket
#   $disconnect — when the client (or server) closes the socket
#   $default    — for every message the client sends
#
# To push a message back to the connected client, POST to riz's
# @connections endpoint with the connection id. RIZ_TEST_BASE_URL
# env override exists so integration tests on ephemeral ports still
# work; production handlers can just use http://localhost:3000.

import os
import urllib.request

BASE_URL = os.environ.get("RIZ_TEST_BASE_URL", "http://localhost:3000")


def lambda_handler(event, _context):
    rc = event.get("requestContext") or {}
    route = rc.get("routeKey")
    conn_id = rc.get("connectionId")

    if route in ("$connect", "$disconnect"):
        return {"statusCode": 200}

    # $default — echo the message back to the sender.
    incoming = event.get("body") or ""
    payload = f"echo: {incoming}".encode("utf-8")
    req = urllib.request.Request(
        f"{BASE_URL}/_riz/connections/{conn_id}",
        data=payload,
        method="POST",
    )
    urllib.request.urlopen(req).read()
    return {"statusCode": 200}
