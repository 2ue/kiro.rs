#!/usr/bin/env node

import http from "node:http";
import { URL } from "node:url";
import { createHash } from "node:crypto";

const DEFAULT_PORT = Number.parseInt(process.env.PORT || process.env.CLAUDE_POOL_MOCK_PORT || "39090", 10);
const DEFAULT_HOST = process.env.HOST || "127.0.0.1";
const DEFAULT_SCENARIO = process.env.CLAUDE_POOL_MOCK_SCENARIO || "success";
const LOG_REQUESTS = process.env.CLAUDE_POOL_MOCK_LOG_REQUESTS === "1";
// Delay before the response headers of Messages, to reproduce a slow upstream.
// `?headerDelayMs=` on the request overrides it.
const DEFAULT_HEADER_DELAY_MS = Number.parseInt(process.env.CLAUDE_POOL_MOCK_HEADER_DELAY_MS || "0", 10);

// Runtime fault control for scheduler tests. `POST /__control` with a JSON patch changes these
// for every following request; `GET /__control` returns them with hit counters.
// - errorRate: share of Messages calls answered with errorStatus (0..1).
// - errorStatus: 429, 500, 502, 503, 403 (403 sends TEMPORARILY_SUSPENDED), or "reset" to drop
//   the connection without a response.
// - failTokens: access tokens (Authorization bearer) that always fail, to fail single accounts.
// - headerDelayMs: delay before response headers.
// - streamChunks / chunkDelayMs: long streams.
// - errorStatus may also name a Anthropic failure: tool_mismatch (500 TOOL_USE_RESULT_MISMATCH),
//   gateway_timeout (524), image_count (400 IMAGE_COUNT_EXCEEDED), quota (402), auth (403).
// - midStreamResetRate: share of long streams cut halfway, after output has started.
// - hangRate / hangMs: share of calls that send response headers and one chunk, then nothing
//   for hangMs (0 = until the caller closes), like an upstream stream that stops responding.
// - trickleRate / trickleIntervalMs / trickleChunks: share of calls that stream trickleChunks
//   chunks trickleIntervalMs apart, like a very long generation that keeps the slot busy.
// `GET /__control` also returns the conversation identity of recent calls.
const control = {
  errorRate: 0,
  errorStatus: 500,
  failTokens: [],
  headerDelayMs: DEFAULT_HEADER_DELAY_MS,
  streamChunks: 0,
  chunkDelayMs: 0,
  midStreamResetRate: 0,
  hangRate: 0,
  hangMs: 0,
  trickleRate: 0,
  trickleIntervalMs: 30000,
  trickleChunks: 20,
};
// - slowTokens / slowExtraMs: extra header delay for the listed tokens, to model slower accounts.
// - jitterMs: random extra header delay 0..jitterMs on every call.
// `GET /__control` also returns, per access token, the calls in flight right now and the highest
// in-flight count and per-minute call count seen since the last reset (`perToken`).
const emptyCounters = () => ({ requests: 0, failed: 0, ok: 0, midStreamResets: 0, hangs: 0, trickles: 0, byToken: {}, conversations: [] });
const counters = { ...emptyCounters(), inFlight: 0, maxInFlight: 0 };
// How late this process runs its own timers. Arrival times and in-flight counts here are taken
// when the event loop gets to a request or a closed socket; on a loaded host that can trail the
// moment claude-pool sent or closed it by seconds, which shows up as calls bunched into one minute
// or a slot counted busy after claude-pool already reused it.
const LOOP_LAG_TICK_MS = 100;
let loopLagExpectedAt = Date.now() + LOOP_LAG_TICK_MS;
setInterval(() => {
  const lag = Math.max(0, Date.now() - loopLagExpectedAt);
  counters.loopLagMaxMs = Math.max(counters.loopLagMaxMs || 0, lag);
  loopLagExpectedAt = Date.now() + LOOP_LAG_TICK_MS;
}, LOOP_LAG_TICK_MS).unref();
// token -> { inFlight, maxInFlight, recent: [call start ms], maxPerMinute }
let perToken = new Map();

function tokenStats(token) {
  let stats = perToken.get(token);
  if (!stats) {
    stats = { inFlight: 0, maxInFlight: 0, recent: [], maxPerMinute: 0, calls: 0 };
    perToken.set(token, stats);
  }
  return stats;
}

function perTokenSnapshot() {
  const now = Date.now();
  const out = {};
  for (const [token, stats] of perToken) {
    stats.recent = stats.recent.filter((at) => now - at < 60000);
    out[token] = {
      inFlight: stats.inFlight,
      maxInFlight: stats.maxInFlight,
      lastMinute: stats.recent.length,
      maxPerMinute: stats.maxPerMinute,
      calls: stats.calls,
    };
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
  return url.searchParams.get("scenario") || process.env.CLAUDE_POOL_MOCK_SCENARIO || DEFAULT_SCENARIO;
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

function event(type, payload) {
  return Buffer.from(`event: ${type}\ndata: ${JSON.stringify({ type, ...payload })}\n\n`);
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
    "This payload comes from the local Anthropic upstream mock.",
  ];
  return pieces.join(" ");
}

function bodyContainsToolResult(body) {
  const raw = JSON.stringify(body || {});
  return raw.includes("tool_result") || raw.includes("toolResult") || raw.includes("toolUseResult") || raw.includes("tool_use_id");
}

function tokenUsageForScenario(scenario, outputTokens) {
  return {
    input_tokens: scenario === "high-cache" ? 198789 : 1234,
    cache_read_input_tokens: 0,
    cache_creation_input_tokens: 0,
    output_tokens: outputTokens,
  };
}

function messageForScenario(body, scenario) {
  let content;
  let stop_reason = "end_turn";
  if ((scenario === "tool-flow" || scenario === "agent-flow") && !bodyContainsToolResult(body)) {
    const agent = scenario === "agent-flow";
    content = [{ type: "tool_use", id: agent ? "toolu_mock_task_1" : "toolu_mock_bash_1",
      name: agent ? "Task" : "Bash", input: agent
        ? { description: "Inspect account selection", prompt: "Inspect src/account/mod.rs without modifying files.", subagent_type: "Explore" }
        : { command: "rg -n account src/model/config.rs | head -40", description: "Inspect account configuration" } }];
    stop_reason = "tool_use";
  } else {
    content = [{ type: "text", text: longAssistantText(body) }];
  }
  return { id: "msg_mock_1", type: "message", role: "assistant", model: body?.model || "claude-sonnet-4-20250514",
    content, stop_reason, stop_sequence: null, usage: tokenUsageForScenario(scenario, 20) };
}

function startFrames(body, scenario) {
  const message = messageForScenario(body, scenario);
  return [{ delayMs: 0, frame: event("message_start", { message: { ...message, content: [], stop_reason: null,
    usage: { ...message.usage, output_tokens: 0 } } }) },
    { delayMs: 0, frame: event("content_block_start", { index: 0, content_block: { type: "text", text: "" } }) }];
}

function textFrame(text) {
  return event("content_block_delta", { index: 0, delta: { type: "text_delta", text } });
}

function endFrames(scenario, outputTokens, stop_reason = "end_turn") {
  return [{ delayMs: 0, frame: event("content_block_stop", { index: 0 }) },
    { delayMs: 0, frame: event("message_delta", { delta: { stop_reason, stop_sequence: null }, usage: tokenUsageForScenario(scenario, outputTokens) }) },
    { delayMs: 0, frame: event("message_stop", {}) }];
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

function buildStreamFrames({ body, scenario }) {
  const message = messageForScenario(body, scenario);
  if (message.stop_reason === "tool_use") {
    const block = message.content[0];
    return [{ delayMs: 0, frame: event("message_start", { message: { ...message, content: [], stop_reason: null } }) },
      { delayMs: 0, frame: event("content_block_start", { index: 0, content_block: { ...block, input: {} } }) },
      { delayMs: 0, frame: event("content_block_delta", { index: 0, delta: { type: "input_json_delta", partial_json: JSON.stringify(block.input) } }) },
      ...endFrames(scenario, 20, "tool_use")];
  }
  const text = scenario === "long-stream" ? Array.from({ length: 24 }, (_, i) => `chunk-${i + 1}`).join(" ") : message.content[0].text;
  const chunks = splitIntoChunks(text, scenario === "long-stream" ? 12 : 3);
  return [...startFrames(body, scenario),
    ...chunks.map((chunk, index) => ({ delayMs: scenario === "slow-stream" ? 180 + index * 25 : 0, frame: textFrame(chunk) })),
    ...endFrames(scenario, Math.max(1, text.length))];
}

function requestWantsConnectionClose(req) {
  return String(req.headers.connection || "").toLowerCase().split(",").map((part) => part.trim()).includes("close");
}

function writeStream(req, res, frames, endDelayMs = 0, resetAtEnd = false) {
  res.writeHead(200, {
    "content-type": "text/event-stream",
    "cache-control": "no-cache",
    connection: requestWantsConnectionClose(req) ? "close" : "keep-alive",
  });
  let index = 0;
  // A hung or trickling call can wait minutes between frames: drop the pending timer as soon as
  // the caller closes, so the mock does not keep timers for calls that are already gone.
  let timer = null;
  res.once("close", () => clearTimeout(timer));
  const writeNext = () => {
    if (index >= frames.length) {
      timer = setTimeout(() => (resetAtEnd ? res.socket?.destroy() : res.end()), endDelayMs);
      return;
    }
    const { delayMs, frame } = frames[index++];
    timer = setTimeout(() => {
      if (res.destroyed) return;
      if (frame) res.write(frame);
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

  json(res, 200, { data: models.map((model) => ({ id: model.id, display_name: model.displayName, type: "model", created_at: "2025-01-01T00:00:00Z" })), has_more: false });
}

function bearerToken(req) {
  const value = String(req.headers.authorization || req.headers["x-api-key"] || "");
  return value.toLowerCase().startsWith("bearer ") ? value.slice(7).trim() : value;
}

// Named upstream failures, shaped like the real Anthropic responses the proxy classifies.
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
    // control.retryAfter: "1" by default; null or "" sends the 429 without the header, like an
    // upstream throttle that does not say when to come back.
    const retryAfter = control.retryAfter === undefined ? "1" : control.retryAfter;
    json(res, 429, { message: "rate limited", reason: "THROTTLED" }, retryAfter ? { "retry-after": String(retryAfter) } : {});
  } else if (code === 403) {
    json(res, 403, { message: "Account temporarily suspended", reason: "TEMPORARILY_SUSPENDED" });
  } else {
    json(res, code || 500, { message: `mock upstream controlled failure ${code}`, requestId: "mock-error" });
  }
}

// Remembers the conversation identity of every Messages call, so a test can
// check that a retry after an upstream failure carries a stable metadata.user_id.
function recordConversation(body, token, outcome) {
  const state = body?.metadata || {};
  counters.conversations.push({
    token,
    outcome,
    conversationId: state.user_id || null,
    agentContinuationId: null,
  });
  if (counters.conversations.length > 5000) counters.conversations.shift();
}

async function handleMessages(req, res, url, scenario, body) {
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
  stats.calls += 1;
  stats.recent = stats.recent.filter((at) => startedAt - at < 60000);
  stats.recent.push(startedAt);
  // The peak uses a window five seconds short of a minute. claude-pool counts a call when it picks
  // the account; the call arrives here after the token check, the Redis round trips and a new
  // upstream connection (it sends `Connection: close`). An account at its RPM takes its next
  // call the moment the oldest one leaves its window, so two calls a minute apart by claude-pool's
  // clock arrive here a minute plus or minus that delay apart, which on a host at load 90 is a
  // few seconds. Two calls let through together still land within this window.
  stats.maxPerMinute = Math.max(stats.maxPerMinute, stats.recent.filter((at) => startedAt - at < 55000).length);
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
  if (!body?.stream) {
    json(res, 200, messageForScenario(body, scenario));
    return;
  }
  if (control.streamChunks > 0) {
    const frames = startFrames(body, scenario);
    for (let index = 0; index < control.streamChunks; index += 1) {
      frames.push({
        delayMs: control.chunkDelayMs,
        frame: textFrame(`chunk-${index} `),
      });
    }
    if (control.hangRate > 0 && Math.random() < control.hangRate) {
      counters.hangs += 1;
      const hangMs = Number(control.hangMs) || 0;
      // One chunk, then silence. With hangMs the connection is cut afterwards (no end of
      // stream); without it the call is held until the caller gives up.
      writeStream(req, res, [frames[0], { delayMs: hangMs > 0 ? hangMs : 2 ** 31 - 1, frame: null }], 0, true);
      return;
    }
    if (control.trickleRate > 0 && Math.random() < control.trickleRate) {
      counters.trickles += 1;
      const chunks = Math.max(1, Number(control.trickleChunks) || 1);
      const slow = startFrames(body, scenario);
      for (let index = 0; index < chunks; index += 1) {
        slow.push({
          delayMs: index === 0 ? 0 : Number(control.trickleIntervalMs) || 0,
          frame: textFrame(`trickle-${index} `),
        });
      }
      slow.push(...endFrames(scenario, chunks * 3));
      writeStream(req, res, slow);
      return;
    }
    // midStreamResetRate: share of streams cut after half the chunks, once output has started.
    if (control.midStreamResetRate > 0 && Math.random() < control.midStreamResetRate) {
      counters.midStreamResets += 1;
      writeStream(req, res, frames.slice(0, Math.max(1, Math.floor(frames.length / 2))), 0, true);
      return;
    }
    frames.push(...endFrames(scenario, control.streamChunks * 3));
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

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url || "/", `http://${req.headers.host || `${DEFAULT_HOST}:${DEFAULT_PORT}`}`);
  const scenario = pickScenario(url);

  res.setHeader("x-mock-scenario", scenario);
  res.setHeader("x-mock-upstream", "pool-loadtest");

  try {
    if (req.method === "GET" && url.pathname === "/v1/models") {
      handleListModels(req, res, url, scenario);
      return;
    }

    if (req.method === "POST" && url.pathname === "/v1/messages") {
      const { raw, json: body } = await parseJsonBody(req);
      logRequest(req, url, scenario, raw, body);
      await handleMessages(req, res, url, scenario, body);
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
            stats.calls = 0;
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

// Keep idle upstream connections open well past Node's 5 s default, like a real upstream; with
// the default, claude-pool reconnects constantly under load and TIME_WAIT eats the local ports.
server.keepAliveTimeout = 120000;
server.headersTimeout = 125000;

server.listen(DEFAULT_PORT, DEFAULT_HOST, () => {
  console.log(`anthropic mock upstream listening on http://${DEFAULT_HOST}:${server.address().port}`);
  console.log(`default scenario: ${DEFAULT_SCENARIO}`);
});
