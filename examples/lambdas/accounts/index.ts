export const handler = async (event: any, _ctx: any) => {
  const id = event.pathParameters?.id ?? "unknown";

  // Parse rawQueryString: "include=profile&verbose=true" → { include: "profile", verbose: "true" }
  const params: Record<string, string> = {};
  if (event.rawQueryString) {
    for (const pair of event.rawQueryString.split("&")) {
      const [k, v] = pair.split("=");
      if (k) params[decodeURIComponent(k)] = decodeURIComponent(v ?? "");
    }
  }

  const account = {
    id,
    name: `Account ${id}`,
    plan: "pro",
    include: params.include ?? null,
    ts: Date.now(),
  };

  return {
    statusCode: 200,
    headers: { "content-type": "application/json" },
    body: JSON.stringify(account),
  };
};
