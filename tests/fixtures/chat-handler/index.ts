// Test-only WebSocket fixture handler.
// Mirrors examples/lambdas/chat/index.ts but reads RIZ_TEST_BASE_URL so the
// @connections POST hits the integration test's dynamic port instead of the
// hardcoded :3000 used in the user-facing example.

const BASE = process.env.RIZ_TEST_BASE_URL || "http://localhost:3000";

export const handler = async (event: any) => {
  const route = event.requestContext.routeKey;
  const id = event.requestContext.connectionId;

  if (route === "$connect" || route === "$disconnect") {
    return { statusCode: 200 };
  }

  // $default: echo the message back to the sender.
  const incoming = event.body ?? "";
  await fetch(`${BASE}/_riz/connections/${id}`, {
    method: "POST",
    body: `echo: ${incoming}`,
  });
  return { statusCode: 200 };
};
