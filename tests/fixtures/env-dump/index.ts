// Test fixture: reflects the worker's own process environment so a test can
// prove env scrubbing — no daemon secret should appear here.
export const handler = async (_event: any, _ctx: any) => ({
  statusCode: 200,
  headers: { "content-type": "application/json" },
  body: JSON.stringify({ env: process.env }),
});
