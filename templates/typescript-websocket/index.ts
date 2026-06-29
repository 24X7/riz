// AWS API Gateway v2 WebSocket Lambda handler.
// Runs on riz (https://riz.dev) via the embedded Bun runtime.
//
// Three lifecycle events arrive at this single handler, distinguished
// by event.requestContext.routeKey:
//   $connect    — when a client opens the socket
//   $disconnect — when the client (or server) closes the socket
//   $default    — for every message the client sends
//
// To push a message back to the connected client, POST to riz's
// @connections endpoint with the connection id. The base URL is
// configurable via RIZ_TEST_BASE_URL so integration tests on
// ephemeral ports still work.

const BASE = process.env.RIZ_TEST_BASE_URL || "http://localhost:3000";

export const handler = async (event: any, _ctx: any) => {
  const route = event?.requestContext?.routeKey;
  const id = event?.requestContext?.connectionId;

  if (route === "$connect" || route === "$disconnect") {
    return { statusCode: 200 };
  }

  // $default — echo the message back to the sender.
  const incoming = event.body ?? "";
  await fetch(`${BASE}/_riz/connections/${id}`, {
    method: "POST",
    body: `echo: ${incoming}`,
  });
  return { statusCode: 200 };
};
