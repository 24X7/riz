// Example WebSocket handler. Receives all three AWS lifecycle event types:
//   $connect    — when a client opens the socket
//   $default    — for every message the client sends
//   $disconnect — when the client (or server) closes the socket
//
// To push a message back to the connected client, the handler POSTs to
// the local @connections management endpoint:
//   POST http://localhost:3000/_riz/connections/{connectionId}
//   body: the raw message bytes

export const handler = async (event: any) => {
  const route = event.requestContext.routeKey;
  const id = event.requestContext.connectionId;

  if (route === "$connect") {
    console.log(`client ${id} connecting`);
    return { statusCode: 200 };
  }

  if (route === "$disconnect") {
    console.log(`client ${id} disconnected`);
    return { statusCode: 200 };
  }

  // $default: echo the message back to the sender.
  const incoming = event.body ?? "";
  await fetch(`http://localhost:3000/_riz/connections/${id}`, {
    method: "POST",
    body: `echo: ${incoming}`,
  });
  return { statusCode: 200 };
};
