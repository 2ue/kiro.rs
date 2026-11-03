#!/usr/bin/env node
// Anthropic-compatible mock external pool for scheduler and fallback tests.
//
// POST /v1/messages answers like Anthropic (JSON or SSE when "stream": true).
// POST /__control with a JSON patch changes behaviour for following requests:
//   errorRate (0..1), errorStatus (429/500/502/503/529 or "reset"), headerDelayMs,
//   streamChunks, chunkDelayMs. GET /__control returns the control and hit counters.

import http from "node:http";

const PORT = Number.parseInt(process.env.PORT || "39190", 10);
const HOST = process.env.HOST || "127.0.0.1";
const NAME = process.env.POOL_NAME || "mock-external";

const control = {
  errorRate: 0,
  errorStatus: 503,
  headerDelayMs: Number.parseInt(process.env.HEADER_DELAY_MS || "0", 10),
  streamChunks: 4,
  chunkDelayMs: 0,
};
const counters = { requests: 0, ok: 0, failed: 0, inFlight: 0, maxInFlight: 0 };

function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    req.on("data", (chunk) => chunks.push(chunk));
    req.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
    req.on("error", reject);
  });
}

function json(res, status, body, headers = {}) {
  const payload = Buffer.from(JSON.stringify(body));
  res.writeHead(status, { "content-type": "application/json", "content-length": payload.length, ...headers });
  res.end(payload);
}

function sse(res, events, chunkDelayMs) {
  res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
  let index = 0;
  const next = () => {
    if (index >= events.length) {
      res.end();
      return;
    }
    const [name, data] = events[index++];
    res.write(`event: ${name}\ndata: ${JSON.stringify(data)}\n\n`);
    setTimeout(next, chunkDelayMs);
  };
  next();
}

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url || "/", `http://${HOST}:${PORT}`);
  if (url.pathname === "/__control") {
    if (req.method === "POST") {
      const patch = JSON.parse((await readBody(req)) || "{}");
      Object.assign(control, patch);
      if (patch.resetCounters) {
        Object.assign(counters, { requests: 0, ok: 0, failed: 0, maxInFlight: counters.inFlight });
      }
    }
    json(res, 200, { name: NAME, control, counters });
    return;
  }
  if (req.method !== "POST" || !url.pathname.endsWith("/v1/messages")) {
    json(res, 404, { type: "error", error: { type: "not_found_error", message: url.pathname } });
    return;
  }
  const raw = await readBody(req);
  let body = {};
  try {
    body = JSON.parse(raw);
  } catch {
    json(res, 400, { type: "error", error: { type: "invalid_request_error", message: "bad json" } });
    return;
  }
  counters.requests += 1;
  counters.inFlight += 1;
  counters.maxInFlight = Math.max(counters.maxInFlight, counters.inFlight);
  res.on("close", () => {
    counters.inFlight -= 1;
  });
  if (control.headerDelayMs > 0) {
    await new Promise((resolve) => setTimeout(resolve, control.headerDelayMs));
    if (res.destroyed || res.writableEnded) return;
  }
  if (control.errorRate > 0 && Math.random() < control.errorRate) {
    counters.failed += 1;
    if (control.errorStatus === "reset") {
      res.socket?.destroy();
      return;
    }
    const status = Number(control.errorStatus) || 503;
    const type = status === 429 ? "rate_limit_error" : status === 529 ? "overloaded_error" : "api_error";
    json(res, status, { type: "error", error: { type, message: `${NAME} controlled failure` } }, status === 429 ? { "retry-after": "1" } : {});
    return;
  }
  counters.ok += 1;
  const model = body.model || "claude-sonnet-4-5";
  const id = `msg_${NAME}_${counters.requests}`;
  const usage = { input_tokens: 12, output_tokens: control.streamChunks * 2, cache_creation_input_tokens: 0, cache_read_input_tokens: 0 };
  if (!body.stream) {
    json(res, 200, {
      id,
      type: "message",
      role: "assistant",
      model,
      content: [{ type: "text", text: `${NAME}-ok` }],
      stop_reason: "end_turn",
      stop_sequence: null,
      usage,
    });
    return;
  }
  const events = [
    ["message_start", { type: "message_start", message: { id, type: "message", role: "assistant", model, content: [], stop_reason: null, stop_sequence: null, usage: { ...usage, output_tokens: 1 } } }],
    ["content_block_start", { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } }],
  ];
  for (let index = 0; index < control.streamChunks; index += 1) {
    events.push(["content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: `${NAME}-${index} ` } }]);
  }
  events.push(["content_block_stop", { type: "content_block_stop", index: 0 }]);
  events.push(["message_delta", { type: "message_delta", delta: { stop_reason: "end_turn", stop_sequence: null }, usage: { output_tokens: usage.output_tokens } }]);
  events.push(["message_stop", { type: "message_stop" }]);
  sse(res, events, control.chunkDelayMs);
});

server.listen(PORT, HOST, () => {
  console.log(`external pool mock ${NAME} listening on http://${HOST}:${PORT}`);
});
