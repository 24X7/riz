# python-websocket riz template

A minimal AWS API Gateway v2 WebSocket Lambda handler in Python,
ready to run on riz (https://riz.dev) via the embedded Python adapter.

## Try it

```bash
riz run
# In another terminal — any WS client works. Example with websocat:
#   websocat ws://localhost:3000/chat
#   > hello
#   < echo: hello
```

## How the lifecycle flows

AWS API Gateway v2 sends three different "route key" events to the
single function, distinguished by `event["requestContext"]["routeKey"]`:

| routeKey | When |
|---|---|
| `$connect`    | A client just opened the WebSocket. Return non-2xx to refuse. |
| `$default`    | The client sent a message. `event["body"]` holds the payload. |
| `$disconnect` | The client (or server) closed the WebSocket.                 |

To push a message back to a connected client, POST to the
`@connections` management endpoint with the connection id. The
template uses `urllib.request` from the standard library so there
are no external Python dependencies.

You can also `DELETE /_riz/connections/{id}` to disconnect a client
from the server side, or `GET /_riz/connections` to list every live
connection.
