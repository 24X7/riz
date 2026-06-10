// Fixture: sleeps for 3000ms before responding.
// Used by integration_timeout tests to guarantee the integration timeout fires
// well before the handler responds — the wide margin (timeout 200ms vs 3000ms
// sleep) makes the 504 deterministic even under heavy parallel test load.
// The test returns as soon as the 504 arrives (~200ms), so the long sleep does
// not slow the suite.
export const handler = async (event: any, _ctx: any) => {
  await new Promise((resolve) => setTimeout(resolve, 3000));
  return {
    statusCode: 200,
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ slept: true }),
  };
};
