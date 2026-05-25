// Bridges AWS Lambda HTTP API Gateway v2 handler → riz stdin/stdout protocol.
// Spawned by riz as: bun run bun-adapter.mjs <handler_path>
//
// Wire format on stdin:  one JSON-encoded `aws_lambda_events::apigw::ApiGatewayV2httpRequest`
//                        per line (the canonical AWS HTTP API GW v2 event shape).
// Wire format on stdout: one JSON-encoded `ApiGatewayV2httpResponse` per line
//                        (statusCode, headers, multiValueHeaders, body, isBase64Encoded, cookies).
//
// The handler signature matches real AWS Lambda:
//   exports.handler = async (event, context) => ({ statusCode, headers, body, ... })
import { createInterface } from "readline";

// Redirect console output to stderr so it doesn't corrupt the stdout protocol stream.
const _toStderr = (...args) => process.stderr.write(args.map(String).join(' ') + '\n');
console.log = console.info = console.debug = _toStderr;

const handlerPath = process.argv[2];
if (!handlerPath) {
  process.stderr.write("riz bun-adapter: missing handler path\n");
  process.exit(1);
}

const mod = await import(handlerPath);
const handler = mod.handler ?? mod.default;
if (typeof handler !== "function") {
  process.stderr.write(
    `riz bun-adapter: no exported 'handler' function in ${handlerPath}\n`
  );
  process.exit(1);
}

const rl = createInterface({ input: process.stdin, terminal: false });

rl.on("line", async (line) => {
  let event;
  try {
    event = JSON.parse(line);
  } catch {
    // Emit a canonical v2 response with a JSON error body.
    process.stdout.write(JSON.stringify({
      statusCode: 400,
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ message: "bad event json" }),
      isBase64Encoded: false,
      cookies: [],
    }) + "\n");
    return;
  }

  // AWS Lambda context object — same shape as the real runtime.
  const context = {
    functionName: process.env.AWS_LAMBDA_FUNCTION_NAME ?? "riz",
    functionVersion: "$LATEST",
    invokedFunctionArn: process.env.AWS_LAMBDA_FUNCTION_ARN ?? "",
    memoryLimitInMB: process.env.AWS_LAMBDA_FUNCTION_MEMORY_SIZE ?? "512",
    awsRequestId: event?.requestContext?.requestId ?? crypto.randomUUID(),
    logGroupName: process.env.AWS_LAMBDA_LOG_GROUP_NAME ?? "/riz",
    logStreamName: process.env.AWS_LAMBDA_LOG_STREAM_NAME ?? "local",
    getRemainingTimeInMillis: () => 30000,
    done: () => {},
    fail: () => {},
    succeed: () => {},
  };

  try {
    const result = await handler(event, context);
    // Default empty response if the handler returned nothing.
    const r = result ?? { statusCode: 204 };
    // Normalize to the canonical AWS response shape so the Rust side can
    // deserialize it into ApiGatewayV2httpResponse cleanly.
    const safeResult = {
      statusCode: typeof r.statusCode === "number" ? r.statusCode : 200,
      headers: r.headers && typeof r.headers === "object" ? r.headers : {},
      multiValueHeaders: r.multiValueHeaders && typeof r.multiValueHeaders === "object"
        ? r.multiValueHeaders : {},
      body: typeof r.body === "string" ? r.body : (r.body == null ? "" : String(r.body)),
      isBase64Encoded: r.isBase64Encoded === true,
      cookies: Array.isArray(r.cookies) ? r.cookies : [],
    };
    process.stdout.write(JSON.stringify(safeResult) + "\n");
  } catch (err) {
    process.stdout.write(JSON.stringify({
      statusCode: 500,
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ message: String(err?.message ?? err) }),
      isBase64Encoded: false,
      cookies: [],
    }) + "\n");
  }
});
