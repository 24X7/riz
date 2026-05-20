export const handler = async (event: any, _ctx: any) => {
  if (!event.body) {
    return {
      statusCode: 400,
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ error: "body required" }),
    };
  }

  let payload: unknown;
  try {
    payload = JSON.parse(event.body);
  } catch {
    return {
      statusCode: 400,
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ error: "body must be valid JSON" }),
    };
  }

  return {
    statusCode: 200,
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ received: payload, confirmedAt: Date.now() }),
  };
};
