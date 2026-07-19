# Python WebSocket handler — mirrors examples/lambdas/chat/index.ts.
# Receives all three AWS lifecycle event types:
#   $connect    — when a client opens the socket
#   $disconnect — when the client (or server) closes the socket
#   $default    — for every message the client sends
#
# To push a message back to the connected client, the handler POSTs to
# the local @connections management endpoint:
#   POST http://localhost:3000/_riz/connections/{connectionId}
#   body: the raw message bytes
#
# The base URL is configurable via RIZ_TEST_BASE_URL so integration tests
# that bind to ephemeral ports can override it.

import json
import os
import urllib.request

BASE_URL = os.environ.get("RIZ_TEST_BASE_URL", "http://localhost:3000")


def lambda_handler(event, _context):
    rc = event.get("requestContext") or {}
    route = rc.get("routeKey")
    conn_id = rc.get("connectionId")

    if route in ("$connect", "$disconnect"):
        return {"statusCode": 200}

    # $default: echo the incoming message back to the sender.
    incoming = event.get("body") or ""
    payload = f"echo: {incoming}".encode("utf-8")
    req = urllib.request.Request(
        f"{BASE_URL}/_riz/connections/{conn_id}",
        data=payload,
        method="POST",
    )
    # We don't care about the response body; just ensure the call completes.
    urllib.request.urlopen(req).read()
    return {"statusCode": 200}
