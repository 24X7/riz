// Fixture: sleeps for 500ms before responding.
// Used by integration_timeout tests to guarantee the integration timeout fires
// before the handler responds.
export const handler = async (event: any, _ctx: any) => {
  await new Promise((resolve) => setTimeout(resolve, 500));
  return {
    statusCode: 200,
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ slept: true }),
  };
};
