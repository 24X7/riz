// Echo handler for the Bun runtime — parity mirror of echo-python and echo-rust.
// All three runtimes must emit IDENTICAL response shape so runtime_parity_echo
// can prove wire-protocol conformance.

export const handler = async (event: any, context: any) => {
  return {
    statusCode: 200,
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
    }),
  };
};
