export const handler = async (_event: any, _ctx: any) => {
  return {
    statusCode: 200,
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ status: "ok", ts: Date.now() }),
  };
};
