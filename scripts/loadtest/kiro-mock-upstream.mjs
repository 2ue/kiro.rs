#!/usr/bin/env node

import http from "node:http";
import { URL } from "node:url";
import { createHash } from "node:crypto";

const DEFAULT_PORT = Number.parseInt(process.env.PORT || process.env.KIRO_MOCK_PORT || "39090", 10);
const DEFAULT_HOST = process.env.HOST || "127.0.0.1";
const DEFAULT_SCENARIO = process.env.KIRO_MOCK_SCENARIO || "success";
const LOG_REQUESTS = process.env.KIRO_MOCK_LOG_REQUESTS === "1";

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
            tokenUsage: {
              uncachedInputTokens: 1234,
              cacheReadInputTokens: 0,
              cacheWriteInputTokens: 0,
              outputTokens: 20,
              totalTokens: 1254,
            },
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
        tokenUsage: {
          uncachedInputTokens: 1234,
          cacheReadInputTokens: 0,
          cacheWriteInputTokens: 0,
          outputTokens: Math.max(1, chunks.join("").length),
          totalTokens: 1234 + Math.max(1, chunks.join("").length),
        },
      }
    ),
  });
  return frames;
}

function requestWantsConnectionClose(req) {
  return String(req.headers.connection || "").toLowerCase().split(",").map((part) => part.trim()).includes("close");
}

function writeStream(req, res, frames, endDelayMs = 0) {
  res.writeHead(200, {
    "content-type": "application/vnd.amazon.eventstream",
    "cache-control": "no-cache",
    connection: requestWantsConnectionClose(req) ? "close" : "keep-alive",
  });
  let index = 0;
  const writeNext = () => {
    if (index >= frames.length) {
      setTimeout(() => res.end(), endDelayMs);
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

function handleGenerateAssistantResponse(req, res, url, scenario, body) {
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
