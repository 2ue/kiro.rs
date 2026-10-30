#!/usr/bin/env node

import http from "node:http";
import { URL } from "node:url";
import { createHash } from "node:crypto";

const DEFAULT_PORT = Number.parseInt(process.env.PORT || process.env.KIRO_MOCK_PORT || "39090", 10);
const DEFAULT_HOST = process.env.HOST || "127.0.0.1";
const DEFAULT_SCENARIO = process.env.KIRO_MOCK_SCENARIO || "success";
const LOG_REQUESTS = process.env.KIRO_MOCK_LOG_REQUESTS === "1";
// Delay before the response headers of generateAssistantResponse, to reproduce a slow upstream.
// `?headerDelayMs=` on the request overrides it.
const DEFAULT_HEADER_DELAY_MS = Number.parseInt(process.env.KIRO_MOCK_HEADER_DELAY_MS || "0", 10);

// Runtime fault control for scheduler tests. `POST /__control` with a JSON patch changes these
// for every following request; `GET /__control` returns them with hit counters.
// - errorRate: share of generateAssistantResponse calls answered with errorStatus (0..1).
// - errorStatus: 429, 500, 502, 503, 403 (403 sends TEMPORARILY_SUSPENDED), or "reset" to drop
//   the connection without a response.
// - failTokens: access tokens (Authorization bearer) that always fail, to fail single accounts.
// - headerDelayMs: delay before response headers.
// - streamChunks / chunkDelayMs: long streams.
// - errorStatus may also name a Kiro failure: tool_mismatch (500 TOOL_USE_RESULT_MISMATCH),
//   gateway_timeout (524), image_count (400 IMAGE_COUNT_EXCEEDED), quota (402), auth (403).
// - midStreamResetRate: share of long streams cut halfway, after output has started.
// `GET /__control` also returns the conversation identity of recent calls.
const control = {
  errorRate: 0,
  errorStatus: 500,
  failTokens: [],
  headerDelayMs: DEFAULT_HEADER_DELAY_MS,
  streamChunks: 0,
  chunkDelayMs: 0,
  midStreamResetRate: 0,
};
// - slowTokens / slowExtraMs: extra header delay for the listed tokens, to model slower accounts.
// - jitterMs: random extra header delay 0..jitterMs on every call.
// `GET /__control` also returns, per access token, the calls in flight right now and the highest
// in-flight count and per-minute call count seen since the last reset (`perToken`).
const emptyCounters = () => ({ requests: 0, failed: 0, ok: 0, midStreamResets: 0, byToken: {}, conversations: [] });
const counters = { ...emptyCounters(), inFlight: 0, maxInFlight: 0 };
// token -> { inFlight, maxInFlight, recent: [call start ms], maxPerMinute }
let perToken = new Map();

function tokenStats(token) {
  let stats = perToken.get(token);
  if (!stats) {
    stats = { inFlight: 0, maxInFlight: 0, recent: [], maxPerMinute: 0 };
    perToken.set(token, stats);
  }
  return stats;
}

function perTokenSnapshot() {
  const now = Date.now();
  const out = {};
  for (const [token, stats] of perToken) {
    stats.recent = stats.recent.filter((at) => now - at < 60000);
    out[token] = { inFlight: stats.inFlight, maxInFlight: stats.maxInFlight, lastMinute: stats.recent.length, maxPerMinute: stats.maxPerMinute };
  }
  return out;
}

const models = [
  { id: "sonnet", displayName: "Sonnet", maxInputTokens: 200000 },
  { id: "claude-sonnet-4-20250514", displayName: "Claude Sonnet 4", maxInputTokens: 200000 },
  { id: "claude-3.7-sonnet", displayName: "Claude 3.7 Sonnet", maxInputTokens: 200000 },
];

function nowIso() {
  return new Date().toISOString();
}

function pickScenario(url) {
  return url.searchParams.get("scenario") || process.env.KIRO_MOCK_SCENARIO || DEFAULT_SCENARIO;
}

function json(res, status, body, headers = {}) {
  const payload = Buffer.from(JSON.stringify(body));
  res.writeHead(status, {
    "content-type": "application/json; charset=utf-8",
    "content-length": payload.length,
    ...headers,
  });
  res.end(payload);
}

function parseJsonBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    req.on("data", (chunk) => chunks.push(chunk));
    req.on("end", () => {
      const raw = Buffer.concat(chunks).toString("utf8");
      if (!raw) {
        resolve({ raw, json: null });
        return;
      }
      try {
        resolve({ raw, json: JSON.parse(raw) });
      } catch (error) {
        reject(Object.assign(new Error(`invalid json body: ${error.message}`), { raw }));
      }
    });
    req.on("error", reject);
  });
}

function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let i = 0; i < 8; i += 1) {
      const mask = -(crc & 1);
      crc = (crc >>> 1) ^ (0xedb88320 & mask);
    }
  }
  return (~crc) >>> 0;
}

function encodeHeaderValue(value) {
  if (typeof value === "string") {
    const data = Buffer.from(value, "utf8");
    return Buffer.concat([Buffer.from([7]), Buffer.from([(data.length >>> 8) & 0xff, data.length & 0xff]), data]);
  }
  if (typeof value === "boolean") {
    return Buffer.from([value ? 0 : 1]);
  }
  if (Number.isInteger(value)) {
    const buf = Buffer.alloc(5);
    buf[0] = 4;
    buf.writeInt32BE(value, 1);
    return buf;
  }
  throw new Error(`unsupported header value: ${value}`);
}

function encodeHeaders(headers) {
  const parts = [];
  for (const [name, value] of Object.entries(headers)) {
    const nameBytes = Buffer.from(name, "utf8");
    parts.push(Buffer.from([nameBytes.length]));
    parts.push(nameBytes);
    parts.push(encodeHeaderValue(value));
  }
  return Buffer.concat(parts);
}

function encodeEventStreamMessage({ headers, payload }) {
  const headerBytes = encodeHeaders(headers);
  const payloadBytes = Buffer.isBuffer(payload) ? payload : Buffer.from(payload, "utf8");
  const totalLength = 12 + headerBytes.length + payloadBytes.length + 4;
  const frame = Buffer.alloc(totalLength);
  frame.writeUInt32BE(totalLength, 0);
  frame.writeUInt32BE(headerBytes.length, 4);
  frame.writeUInt32BE(crc32(frame.subarray(0, 8)), 8);
  headerBytes.copy(frame, 12);
  payloadBytes.copy(frame, 12 + headerBytes.length);
  frame.writeUInt32BE(crc32(frame.subarray(0, totalLength - 4)), totalLength - 4);
  return frame;
}

function event(headers, payload) {
  return encodeEventStreamMessage({
    headers: {
      ":message-type": "event",
      ":event-type": headers.eventType,
      ...headers.extra,
    },
    payload: JSON.stringify(payload),
  });
}

function splitIntoChunks(text, chunkCount) {
  if (chunkCount <= 1 || text.length <= 1) {
    return [text];
  }
  const size = Math.ceil(text.length / chunkCount);
  const chunks = [];
  for (let i = 0; i < text.length; i += size) {
    chunks.push(text.slice(i, i + size));
  }
  return chunks;
}

function longAssistantText(body) {
  const seed = createHash("sha256").update(JSON.stringify(body || {})).digest("hex").slice(0, 16);
  const pieces = [
    "Mock assistant response.",
    `Scenario seed: ${seed}.`,
    "This payload comes from the local Kiro upstream mock.",
  ];
  return pieces.join(" ");
}

function bodyContainsToolResult(body) {
  const raw = JSON.stringify(body || {});
  return raw.includes("tool_result") || raw.includes("toolResult") || raw.includes("toolUseResult") || raw.includes("tool_use_id");
}

function tokenUsageForScenario(scenario, outputTokens) {
  if (scenario === "high-cache") {
    return {
      uncachedInputTokens: 198789,
      cacheReadInputTokens: 0,
      cacheWriteInputTokens: 0,
      outputTokens,
      totalTokens: 198789 + outputTokens,
    };
  }

  return {
    uncachedInputTokens: 1234,
    cacheReadInputTokens: 0,
    cacheWriteInputTokens: 0,
    outputTokens,
    totalTokens: 1234 + outputTokens,
  };
}

function logRequest(req, url, scenario, raw, body) {
  if (!LOG_REQUESTS) return;
  const compact = raw ? raw.replace(/\s+/g, " ").slice(0, 500) : "";
  console.log(JSON.stringify({
    time: nowIso(),
    method: req.method,
    path: url.pathname,
    scenario,
    rawBytes: Buffer.byteLength(raw || ""),
    hasToolResult: bodyContainsToolResult(body),
    preview: compact,
  }));
}

function toolFlowEvent(scenario) {
  if (scenario === "agent-flow") {
    return event(
      { eventType: "toolUseEvent" },
      {
        name: "Task",
        toolUseId: "toolu_mock_task_1",
        input: JSON.stringify({
          description: "Inspect token manager hot path",
          prompt: "Search src/kiro/token_manager.rs for pending_stats_deltas and summarize what it does. Do not modify files.",
          subagent_type: "Explore",
        }),
        stop: true,
      }
    );
  }
  return event(
    { eventType: "toolUseEvent" },
    {
      name: "Bash",
      toolUseId: "toolu_mock_bash_1",
      input: JSON.stringify({
        command: "rg -n \"pending_stats_deltas|kiro_upstream_base_url|profileArn\" src/kiro src/model/config.rs | head -40",
        description: "Search Kiro protocol and scheduler hot paths",
      }),
      stop: true,
    }
  );
}

function buildStreamFrames({ body, scenario }) {
  if ((scenario === "tool-flow" || scenario === "agent-flow") && !bodyContainsToolResult(body)) {
    return [
      { delayMs: 0, frame: toolFlowEvent(scenario) },
      {
        delayMs: 0,
        frame: event(
          { eventType: "metadataEvent" },
          {
            tokenUsage: tokenUsageForScenario(scenario, 20),
          }
        ),
      },
    ];
  }

  const assistantText = longAssistantText(body);
  const chunks = scenario === "long-stream"
    ? splitIntoChunks(Array.from({ length: 24 }, (_, i) => `chunk-${String(i + 1).padStart(2, "0")}`).join(" "), 12)
    : splitIntoChunks(assistantText, 3);
  const frames = [];
  if (scenario === "slow-stream") {
    frames.push({ delayMs: 250, frame: event({ eventType: "contextUsageEvent" }, { contextUsagePercentage: 13.5 }) });
  }
  for (const [index, chunk] of chunks.entries()) {
    frames.push({
      delayMs: scenario === "slow-stream" ? 180 + index * 25 : 0,
      frame: event({ eventType: "assistantResponseEvent" }, { content: chunk }),
    });
  }
  frames.push({
    delayMs: scenario === "slow-stream" ? 80 : 0,
    frame: event(
      { eventType: "metadataEvent" },
      {
        tokenUsage: tokenUsageForScenario(scenario, Math.max(1, chunks.join("").length)),
      }
    ),
  });
  return frames;
}

function requestWantsConnectionClose(req) {
  return String(req.headers.connection || "").toLowerCase().split(",").map((part) => part.trim()).includes("close");
}

function writeStream(req, res, frames, endDelayMs = 0, resetAtEnd = false) {
  res.writeHead(200, {
    "content-type": "application/vnd.amazon.eventstream",
    "cache-control": "no-cache",
    connection: requestWantsConnectionClose(req) ? "close" : "keep-alive",
  });
  let index = 0;
  const writeNext = () => {
    if (index >= frames.length) {
      setTimeout(() => (resetAtEnd ? res.socket?.destroy() : res.end()), endDelayMs);
      return;
    }
    const { delayMs, frame } = frames[index++];
    setTimeout(() => {
      res.write(frame);
      writeNext();
    }, delayMs);
  };
  writeNext();
}

function handleListModels(req, res, url, scenario) {
  if (scenario === "429") {
    json(res, 429, { message: "rate limited", reason: "THROTTLED" }, { "retry-after": "1" });
    return;
  }
  if (scenario === "500") {
    json(res, 500, { message: "internal error from mock upstream", requestId: "mock-error" });
    return;
  }

  const modelList = models.map((model) => ({
    modelId: model.id,
    modelName: model.displayName,
    supportedInputTypes: ["text"],
    tokenLimits: {
      maxInputTokens: model.maxInputTokens,
      maxOutputTokens: 8192,
    },
  }));

  json(res, 200, {
    models: modelList,
    nextToken: null,
    requestId: `mock-${url.pathname.replace(/\//g, "-")}`,
    scenario,
  });
}

function bearerToken(req) {
  const value = String(req.headers.authorization || "");
  return value.toLowerCase().startsWith("bearer ") ? value.slice(7).trim() : value;
}

// Named upstream failures, shaped like the real Kiro responses the proxy classifies.
const NAMED_FAILURES = {
  tool_mismatch: [500, { message: "Tool use and tool result do not match", reason: "TOOL_USE_RESULT_MISMATCH" }],
  gateway_timeout: [524, { message: "error code: 524" }],
  image_count: [400, { message: "too many inline media segments: 123 exceeds limit 100", reason: "IMAGE_COUNT_EXCEEDED" }],
  quota: [402, { message: "You have reached the limit for monthly requests", reason: "MONTHLY_REQUEST_COUNT" }],
  auth: [403, { message: "The bearer token included in the request is invalid." }],
};

function writeControlledFailure(res, status) {
  if (status === "reset") {
    res.socket?.destroy();
    return;
  }
  if (NAMED_FAILURES[status]) {
    const [code, body] = NAMED_FAILURES[status];
    json(res, code, body);
    return;
  }
  const code = Number(status);
  if (code === 429) {
    json(res, 429, { message: "rate limited", reason: "THROTTLED" }, { "retry-after": "1" });
  } else if (code === 403) {
    json(res, 403, { message: "Account temporarily suspended", reason: "TEMPORARILY_SUSPENDED" });
  } else {
    json(res, code || 500, { message: `mock upstream controlled failure ${code}`, requestId: "mock-error" });
  }
}

// Remembers the conversation identity of every generateAssistantResponse call, so a test can
// check that a retry after an upstream failure carries a fresh conversationId.
function recordConversation(body, token, outcome) {
  const state = body?.conversationState || {};
  counters.conversations.push({
    token,
    outcome,
    conversationId: state.conversationId || null,
    agentContinuationId: state.agentContinuationId || null,
  });
  if (counters.conversations.length > 5000) counters.conversations.shift();
}

async function handleGenerateAssistantResponse(req, res, url, scenario, body) {
  counters.requests += 1;
  counters.inFlight += 1;
  counters.maxInFlight = Math.max(counters.maxInFlight, counters.inFlight);
  res.on("close", () => {
    counters.inFlight -= 1;
  });
  const token = bearerToken(req);
  counters.byToken[token] = (counters.byToken[token] || 0) + 1;
  const stats = tokenStats(token);
  const startedAt = Date.now();
  stats.inFlight += 1;
  stats.maxInFlight = Math.max(stats.maxInFlight, stats.inFlight);
  stats.recent = stats.recent.filter((at) => startedAt - at < 60000);
  stats.recent.push(startedAt);
  stats.maxPerMinute = Math.max(stats.maxPerMinute, stats.recent.length);
  res.on("close", () => {
    stats.inFlight -= 1;
  });
  const slowExtraMs = (control.slowTokens || []).includes(token) ? Number(control.slowExtraMs) || 0 : 0;
  const jitterMs = Number(control.jitterMs) > 0 ? Math.floor(Math.random() * Number(control.jitterMs)) : 0;
  const headerDelayMs =
    Number.parseInt(url.searchParams.get("headerDelayMs") || `${control.headerDelayMs}`, 10) + slowExtraMs + jitterMs;
  if (headerDelayMs > 0) {
    await new Promise((resolve) => setTimeout(resolve, headerDelayMs));
    // The request body has already been read, so only the response side tells whether the
    // client is still there.
    if (res.destroyed || res.writableEnded) {
      return;
    }
  }
  if (control.failTokens.includes(token) || (control.errorRate > 0 && Math.random() < control.errorRate)) {
    counters.failed += 1;
    recordConversation(body, token, "failed");
    writeControlledFailure(res, control.errorStatus);
    return;
  }
  counters.ok += 1;
  recordConversation(body, token, "ok");
  if (control.streamChunks > 0) {
    const frames = [];
    for (let index = 0; index < control.streamChunks; index += 1) {
      frames.push({
        delayMs: control.chunkDelayMs,
        frame: event({ eventType: "assistantResponseEvent" }, { content: `chunk-${index} ` }),
      });
    }
    // midStreamResetRate: share of streams cut after half the chunks, once output has started.
    if (control.midStreamResetRate > 0 && Math.random() < control.midStreamResetRate) {
      counters.midStreamResets += 1;
      writeStream(req, res, frames.slice(0, Math.max(1, Math.floor(frames.length / 2))), 0, true);
      return;
    }
    frames.push({
      delayMs: 0,
      frame: event({ eventType: "metadataEvent" }, { tokenUsage: tokenUsageForScenario(scenario, control.streamChunks * 3) }),
    });
    writeStream(req, res, frames);
    return;
  }
  if (scenario === "429") {
    json(res, 429, { message: "rate limited", reason: "THROTTLED" }, { "retry-after": "1" });
    return;
  }
  if (scenario === "500") {
    json(res, 500, { message: "internal error from mock upstream", requestId: "mock-error" });
    return;
  }
  writeStream(req, res, buildStreamFrames({ body, scenario }));
}

function handleMcp(req, res, url, scenario, body) {
  if (scenario === "429") {
    json(res, 429, { message: "rate limited", reason: "THROTTLED" }, { "retry-after": "1" });
    return;
  }
  if (scenario === "500") {
    json(res, 500, { message: "internal error from mock upstream", requestId: "mock-error" });
    return;
  }

  json(res, 200, {
    jsonrpc: "2.0",
    id: body?.id ?? null,
    result: {
      ok: true,
      scenario,
      method: body?.method ?? "unknown",
      path: url.pathname,
      timestamp: nowIso(),
    },
  });
}

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url || "/", `http://${req.headers.host || `${DEFAULT_HOST}:${DEFAULT_PORT}`}`);
  const scenario = pickScenario(url);

  res.setHeader("x-mock-scenario", scenario);
  res.setHeader("x-mock-upstream", "kiro-loadtest");

  try {
    if (req.method === "GET" && url.pathname === "/ListAvailableModels") {
      handleListModels(req, res, url, scenario);
      return;
    }

    if (req.method === "POST" && url.pathname === "/generateAssistantResponse") {
      const { raw, json: body } = await parseJsonBody(req);
      logRequest(req, url, scenario, raw, body);
      handleGenerateAssistantResponse(req, res, url, scenario, body);
      return;
    }

    if (req.method === "POST" && url.pathname === "/mcp") {
      const { raw, json: body } = await parseJsonBody(req);
      logRequest(req, url, scenario, raw, body);
      handleMcp(req, res, url, scenario, body);
      return;
    }

    if (url.pathname === "/__control") {
      if (req.method === "POST") {
        const { json: patch } = await parseJsonBody(req);
        Object.assign(control, patch || {});
        if (patch && patch.resetCounters) {
          Object.assign(counters, emptyCounters(), { maxInFlight: counters.inFlight });
          for (const stats of perToken.values()) {
            stats.maxInFlight = stats.inFlight;
            stats.maxPerMinute = 0;
          }
        }
      }
      json(res, 200, { control, counters: { ...counters, perToken: perTokenSnapshot() } });
      return;
    }

    if (req.method === "GET" && url.pathname === "/healthz") {
      json(res, 200, { ok: true, scenario, time: nowIso() });
      return;
    }

    json(res, 404, { message: "not found", path: url.pathname, scenario });
  } catch (error) {
    json(res, 500, {
      message: error.message,
      scenario,
      raw: error.raw ?? null,
    });
  }
});

server.on("clientError", (error, socket) => {
  console.log(JSON.stringify({
    time: nowIso(),
    event: "clientError",
    code: error.code || null,
    message: error.message,
    rawPacketPreview: error.rawPacket ? error.rawPacket.toString("utf8", 0, 500) : null,
  }));

  if (!socket.writable) return;
  socket.end(
    "HTTP/1.1 400 Bad Request\r\n" +
      "content-type: application/json; charset=utf-8\r\n" +
      "connection: close\r\n" +
      "\r\n" +
      JSON.stringify({ message: "bad request", reason: error.code || "CLIENT_ERROR" })
  );
});

server.listen(DEFAULT_PORT, DEFAULT_HOST, () => {
  console.log(`kiro mock upstream listening on http://${DEFAULT_HOST}:${DEFAULT_PORT}`);
  console.log(`default scenario: ${DEFAULT_SCENARIO}`);
});
