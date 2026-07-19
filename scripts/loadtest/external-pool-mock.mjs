#!/usr/bin/env node

import http from "node:http";

const host = process.env.HOST || "127.0.0.1";
const port = parseNonNegativeInt(process.env.PORT || "39095", "PORT");
const firstByteDelayMs = parseNonNegativeInt(
  process.env.EXTERNAL_MOCK_FIRST_BYTE_DELAY_MS || "100",
  "EXTERNAL_MOCK_FIRST_BYTE_DELAY_MS",
);
const totalMs = Math.max(
  firstByteDelayMs,
  parseNonNegativeInt(
    process.env.EXTERNAL_MOCK_TOTAL_MS || "10000",
    "EXTERNAL_MOCK_TOTAL_MS",
  ),
);

const state = {
  startedAt: new Date().toISOString(),
  requests: 0,
  completed: 0,
  aborted: 0,
  inFlight: 0,
  peakInFlight: 0,
};

function parseNonNegativeInt(value, name) {
  const parsed = Number.parseInt(String(value), 10);
  if (!Number.isFinite(parsed) || parsed < 0 || String(parsed) !== String(value).trim()) {
    throw new Error(`${name} must be a non-negative integer`);
  }
  return parsed;
}

function sendJson(res, status, value) {
  const payload = Buffer.from(JSON.stringify(value));
  res.writeHead(status, {
    "content-type": "application/json; charset=utf-8",
    "content-length": payload.length,
  });
  res.end(payload);
}

function drainRequest(req) {
  return new Promise((resolve, reject) => {
    req.on("data", () => {});
    req.on("end", resolve);
    req.on("error", reject);
  });
}

function sse(event, data) {
  return `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
}

async function handleMessages(req, res) {
  await drainRequest(req);
  state.requests += 1;
  state.inFlight += 1;
  state.peakInFlight = Math.max(state.peakInFlight, state.inFlight);
  let finished = false;
  const finish = (aborted) => {
    if (finished) return;
    finished = true;
    state.inFlight = Math.max(0, state.inFlight - 1);
    if (aborted) state.aborted += 1;
    else state.completed += 1;
  };
  req.once("aborted", () => finish(true));
  res.once("close", () => {
    if (!res.writableEnded) finish(true);
  });

  res.writeHead(200, {
    "content-type": "text/event-stream; charset=utf-8",
    "cache-control": "no-cache",
    connection: "keep-alive",
    "x-external-pool-mock": "true",
  });
  setTimeout(() => {
    if (finished || res.destroyed) return;
    res.write(
      sse("message_start", {
        type: "message_start",
        message: {
          id: `msg_mock_${state.requests}`,
          type: "message",
          role: "assistant",
          content: [],
          model: "sonnet",
          stop_reason: null,
          usage: { input_tokens: 16, output_tokens: 0 },
        },
      }),
    );
    res.write(
      sse("content_block_delta", {
        type: "content_block_delta",
        index: 0,
        delta: { type: "text_delta", text: "external mock response" },
      }),
    );
  }, firstByteDelayMs);
  setTimeout(() => {
    if (finished || res.destroyed) return;
    res.write(
      sse("message_delta", {
        type: "message_delta",
        delta: { stop_reason: "end_turn", stop_sequence: null },
        usage: { output_tokens: 3 },
      }),
    );
    res.write(sse("message_stop", { type: "message_stop" }));
    res.end();
    finish(false);
  }, totalMs);
}

const server = http.createServer(async (req, res) => {
  try {
    if (req.method === "GET" && req.url === "/healthz") {
      sendJson(res, 200, { ok: true });
      return;
    }
    if (req.method === "GET" && req.url === "/metrics") {
      sendJson(res, 200, { ...state, sampledAt: new Date().toISOString() });
      return;
    }
    if (req.method === "POST") {
      await handleMessages(req, res);
      return;
    }
    sendJson(res, 404, { error: "not_found" });
  } catch (error) {
    if (!res.headersSent) {
      sendJson(res, 500, { error: error.message });
    } else {
      res.destroy(error);
    }
  }
});

server.listen(port, host, () => {
  console.log(
    JSON.stringify({
      event: "listening",
      url: `http://${host}:${port}`,
      firstByteDelayMs,
      totalMs,
    }),
  );
});
