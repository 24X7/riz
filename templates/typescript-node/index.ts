// AWS API Gateway v2 HTTP Lambda handler in TypeScript, run directly by
// Node's native type stripping (Node >= 22.18) — no build step, no bundler.
// Runs on riz (https://riz.dev) via the system `node` runtime.
//
// Types come from @types/aws-lambda (editor-only devDependency — the exact
// AWS Lambda shape; handlers written for real AWS run here unchanged).

import type {
  APIGatewayProxyEventV2,
  APIGatewayProxyResultV2,
  Context,
} from "aws-lambda";

export const handler = async (
  event: APIGatewayProxyEventV2,
  context: Context,
): Promise<APIGatewayProxyResultV2> => {
  const name = event.queryStringParameters?.name ?? "world";
  return {
    statusCode: 200,
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      message: `hello, ${name}`,
      method: event.requestContext.http.method,
      path: event.rawPath,
      functionName: context.functionName,
      awsRequestId: context.awsRequestId,
      remainingMs: context.getRemainingTimeInMillis(),
    }),
  };
};
