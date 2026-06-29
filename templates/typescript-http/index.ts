// AWS API Gateway v2 HTTP Lambda handler.
// Runs on riz (https://riz.dev) via the embedded Bun runtime.

export const handler = async (event: any, context: any) => {
  const name = event?.queryStringParameters?.name ?? "world";
  return {
    statusCode: 200,
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      message: `hello, ${name}`,
      method: event?.requestContext?.http?.method,
      path: event?.rawPath,
      functionName: context.functionName,
      awsRequestId: context.awsRequestId,
      remainingMs: context.getRemainingTimeInMillis(),
    }),
  };
};
