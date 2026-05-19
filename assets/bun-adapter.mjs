// Bridges AWS Lambda HTTP Gateway v2 handler → osbox stdin/stdout protocol.
// Spawned by osbox as: bun run bun-adapter.mjs <handler_path>
import { createInterface } from "readline";

const handlerPath = process.argv[2];
if (!handlerPath) {
  process.stderr.write("osbox bun-adapter: missing handler path\n");
  process.exit(1);
}

const mod = await import(handlerPath);
const handler = mod.handler ?? mod.default;

if (typeof handler !== "function") {
  process.stderr.write(
    `osbox bun-adapter: no exported 'handler' function in ${handlerPath}\n`
  );
  process.exit(1);
}

const rl = createInterface({ input: process.stdin, terminal: false });

rl.on("line", async (line) => {
  let event;
  try {
    event = JSON.parse(line);
  } catch {
    process.stdout.write(
      JSON.stringify({ statusCode: 400, body: "bad event json" }) + "\n"
    );
    return;
  }

  const context = {
    functionName: process.env.AWS_LAMBDA_FUNCTION_NAME ?? "osbox",
    functionVersion: "$LATEST",
    invokedFunctionArn: "",
    memoryLimitInMB: "512",
    awsRequestId: crypto.randomUUID(),
    logGroupName: "/osbox",
    logStreamName: "local",
    getRemainingTimeInMillis: () => 30000,
    done: () => {},
    fail: () => {},
    succeed: () => {},
  };

  try {
    const result = await handler(event, context);
    const safeResult = result ?? { statusCode: 204, body: "" };
    process.stdout.write(JSON.stringify(safeResult) + "\n");
  } catch (err) {
    process.stdout.write(
      JSON.stringify({
        statusCode: 500,
        body: JSON.stringify({ error: String(err) }),
      }) + "\n"
    );
  }
});
