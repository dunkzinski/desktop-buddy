// Evaluate an expression inside the running Tauri WebView2 page over CDP.
// Usage: node cdp-eval.mjs <wsUrl> <expression>
const wsUrl = process.argv[2];
const expression = process.argv[3];
if (!wsUrl || !expression) {
  console.error("usage: node cdp-eval.mjs <wsUrl> <expression>");
  process.exit(2);
}

const ws = new WebSocket(wsUrl);
const REQ_ID = 1;
const timer = setTimeout(() => {
  console.error("timeout waiting for CDP reply");
  process.exit(3);
}, 10000);

ws.onopen = () => {
  ws.send(JSON.stringify({
    id: REQ_ID,
    method: "Runtime.evaluate",
    params: { expression, awaitPromise: true, returnByValue: true },
  }));
};

ws.onmessage = (event) => {
  const msg = JSON.parse(event.data);
  if (msg.id !== REQ_ID) return;
  clearTimeout(timer);
  if (msg.error) {
    console.error("CDP error:", JSON.stringify(msg.error));
    process.exit(4);
  }
  const result = msg.result || {};
  if (result.exceptionDetails) {
    console.error("page exception:", JSON.stringify(result.exceptionDetails, null, 2));
    process.exit(5);
  }
  console.log(JSON.stringify(result.result ? result.result.value : result, null, 2));
  ws.close();
};

ws.onerror = (err) => {
  console.error("ws error:", err && err.message ? err.message : err);
  process.exit(1);
};
