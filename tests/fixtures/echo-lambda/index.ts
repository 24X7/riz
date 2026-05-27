export const handler = async (event: any, ctx: any) => {
  return {
    statusCode: 200,
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      echo: event.rawPath,
      method: event.requestContext.http.method,
      functionName: ctx.functionName,
      invokedFunctionArn: ctx.invokedFunctionArn,
      awsRequestId: ctx.awsRequestId,
      remainingMs: ctx.getRemainingTimeInMillis(),
    }),
  };
};
