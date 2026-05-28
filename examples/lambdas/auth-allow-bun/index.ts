// REQUEST authorizer (allow path). Returns the simple-response format
// expected by AWS API GW v2 HTTP API authorizers:
//   { isAuthorized: bool, context: { ... } }
//
// principalId in context becomes event.requestContext.authorizer.fields.principalId
// for the downstream handler. Extra keys (here: tier) are also merged in.
export const handler = async (_event: any, _ctx: any) => ({
  isAuthorized: true,
  context: {
    principalId: "u42",
    tier: "gold",
  },
});
