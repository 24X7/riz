// Echo handler for the Node.js runtime — parity mirror of echo-bun /
// echo-python / echo-rust. All runtimes must emit an IDENTICAL response shape
// so runtime_parity_echo can prove wire-protocol conformance.
//
// Plain ESM JavaScript (no TypeScript) so it runs on any Node ≥ 14 without a
// build step — `node` loads it directly via the riz node adapter.

export const handler = async (event, context) => {
  // Honor ?status=NNN for the parity-H error-status test.
  const statusOverride = parseInt(event?.queryStringParameters?.status, 10);
  const statusCode = Number.isFinite(statusOverride) ? statusOverride : 200;
  // Per-process invocation counter for the parity-K cache test. Node keeps
  // module-level state across invocations (the process is reused per
  // concurrency slot), so this monotonically increases until the process
  // is replaced. A cache hit replays the prior response — including its
  // captured invocationCount — without re-running the handler.
  globalThis.__invocationCount = (globalThis.__invocationCount ?? 0) + 1;
  const invocationCount = globalThis.__invocationCount;
  return {
    statusCode,
    headers: { "content-type": "application/json", "x-riz-echo": "ok" },
    cookies: ["sid=abc; Path=/"],
    body: JSON.stringify({
      echo: event.rawPath ?? "",
      method: event?.requestContext?.http?.method ?? null,
      functionName: context.functionName,
      invokedFunctionArn: context.invokedFunctionArn,
      awsRequestId: context.awsRequestId,
      remainingMs: context.getRemainingTimeInMillis(),
      body: event.body ?? null,
      isBase64Encoded: event.isBase64Encoded ?? false,
      pathParameters: event.pathParameters ?? null,
      queryStringParameters: event.queryStringParameters ?? null,
      stageVariables: event.stageVariables ?? null,
      cookies: event.cookies ?? null,
      requestHeaders: event.headers ?? null,
      authorizer: event?.requestContext?.authorizer ?? null,
      invocationCount,
    }),
  };
};
