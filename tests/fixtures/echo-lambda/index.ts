export const handler = async (event: any, _ctx: any) => {
  return {
    statusCode: 200,
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ echo: event.rawPath, method: event.requestContext.http.method }),
  };
};
