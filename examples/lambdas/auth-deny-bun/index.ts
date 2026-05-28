// REQUEST authorizer (deny path). Returns isAuthorized: false — riz
// converts this to a 401 Unauthorized response without invoking the
// protected handler.
export const handler = async (_event: any, _ctx: any) => ({
  isAuthorized: false,
});
