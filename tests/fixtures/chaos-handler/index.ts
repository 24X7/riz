// Chaos + perf test fixture. A single Bun handler with query-string knobs the
// harnesses use to drive fault injection and load:
//   ?status=NNN  → return that status code
//   ?sleep=MS    → hold the worker busy MS ms before responding (saturation)
//   ?crash=1     → kill THIS worker mid-invoke (process.exit) — no response;
//                  riz sees a dead worker, respawns, and counts a crash
// With no knobs it is a fast 200 — the perf hot path.
export const handler = async (event: any, context: any) => {
  const q = event?.queryStringParameters ?? {};

  if (q.crash === "1") {
    // Die without answering: the pool observes a crashed worker.
    process.exit(1);
  }

  const sleep = parseInt(q.sleep, 10);
  if (Number.isFinite(sleep) && sleep > 0) {
    await new Promise((r) => setTimeout(r, sleep));
  }

  const status = parseInt(q.status, 10);
  return {
    statusCode: Number.isFinite(status) ? status : 200,
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      ok: true,
      name: q.name ?? "chaos",
      functionName: context.functionName,
      awsRequestId: context.awsRequestId,
    }),
  };
};
