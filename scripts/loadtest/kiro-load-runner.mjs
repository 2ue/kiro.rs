#!/usr/bin/env node

import http from "node:http";
import https from "node:https";
import { URL } from "node:url";
import { setTimeout as sleep } from "node:timers/promises";
import { createHash, randomUUID } from "node:crypto";

const args = parseArgs(process.argv.slice(2));
const baseUrl = new URL(args.baseUrl || process.env.KIRO_BASE_URL || "http://127.0.0.1:9022");
const path = args.path || process.env.KIRO_MESSAGES_PATH || "/cc/v1/messages";
const durationMs = parseDuration(args.duration || process.env.DURATION || "30s");
const concurrency = Number.parseInt(args.concurrency || process.env.CONCURRENCY || "20", 10);
const targetRpm = Number.parseInt(args.rpm || process.env.RPM || "2000", 10);
const streamMode = parseBool(args.stream ?? process.env.STREAM ?? "true");
const scenario = args.scenario || process.env.KIRO_MOCK_SCENARIO || "success";
const apiKey = args.apiKey || process.env.KIRO_API_KEY || "sk-kiro-rs-local-debug";
const noSummary = parseBool(args.noSummary ?? process.env.NO_SUMMARY ?? "false");
const conversationMode = args.conversationMode || process.env.CONVERSATION_MODE || "derived";

const agent = baseUrl.protocol === "https:" ? new https.Agent({ keepAlive: true, maxSockets: concurrency * 2 }) : new http.Agent({ keepAlive: true, maxSockets: concurrency * 2 });

const sampleBody = {
  model: "sonnet",
  max_tokens: 256,
  stream: streamMode,
  messages: [{ role: "user", content: "Return a short deterministic answer for load testing." }],
};

const state = {
  startedAt: Date.now(),
  sent: 0,
  ok: 0,
  failed: 0,
  statuses: new Map(),
  errors: new Map(),
  latencies: [],
  streamLatencies: [],
  duplicateResponses: 0,
  duplicateChunks: 0,
  responseDigests: new Map(),
  inFlight: 0,
  firstByteLatencies: [],
};

function parseArgs(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i += 1) {
    const item = argv[i];
    if (!item.startsWith("--")) continue;
    const rawKey = item.slice(2);
    const key = rawKey.replace(/-([a-z])/g, (_, ch) => ch.toUpperCase());
    const next = argv[i + 1];
    if (!next || next.startsWith("--")) {
      out[key] = "true";
    } else {
      out[key] = next;
      i += 1;
    }
  }
  return out;
}

function parseBool(value) {
  return value === true || value === "true" || value === "1" || value === "yes";
}

function parseDuration(value) {
  if (typeof value === "number") return value;
  const match = String(value).trim().match(/^(\d+(?:\.\d+)?)(ms|s|m)?$/i);
  if (!match) throw new Error(`invalid duration: ${value}`);
  const amount = Number.parseFloat(match[1]);
  const unit = (match[2] || "s").toLowerCase();
  if (unit === "ms") return amount;
  if (unit === "s") return amount * 1000;
  if (unit === "m") return amount * 60_000;
  throw new Error(`invalid duration unit: ${unit}`);
}

function percentile(values, p) {
  if (values.length === 0) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  const idx = Math.min(sorted.length - 1, Math.max(0, Math.ceil((p / 100) * sorted.length) - 1));
  return sorted[idx];
}

function inc(map, key) {
  map.set(key, (map.get(key) || 0) + 1);
}

function digest(text) {
  return createHash("sha256").update(text).digest("hex");
}

function encodeRequest(pathname) {
  return `${baseUrl.origin}${pathname}`;
}

function requestBodyForSequence(sequence) {
  const body = structuredClone(sampleBody);
  if (conversationMode === "unique") {
    body.metadata = {
      user_id: JSON.stringify({
        device_id: "kiro-loadtest",
        account_uuid: "loadtest",
        session_id: randomUUID(),
        request_sequence: sequence,
      }),
    };
  }
  return body;
}

function makeRequest(method, pathname, body) {
  const url = new URL(encodeRequest(pathname));
  if (scenario && pathname === path) {
    url.searchParams.set("scenario", scenario);
  }
  const payload = body ? Buffer.from(JSON.stringify(body)) : null;
  const headers = {
    "content-type": "application/json",
    accept: "application/vnd.amazon.eventstream, application/json",
    "x-api-key": apiKey,
    "user-agent": "kiro-loadtest/1.0",
  };
  if (payload) headers["content-length"] = String(payload.length);
  return { url, headers, payload };
}

function decodeEventStream(buffer) {
  const events = [];
  let offset = 0;
  while (buffer.length - offset >= 12) {
    const totalLength = buffer.readUInt32BE(offset);
    if (buffer.length - offset < totalLength) break;
    const headerLength = buffer.readUInt32BE(offset + 4);
    const payloadStart = offset + 12 + headerLength;
    const payloadEnd = offset + totalLength - 4;
    const headers = parseHeaders(buffer.subarray(offset + 12, payloadStart));
    const payload = buffer.subarray(payloadStart, payloadEnd).toString("utf8");
    events.push({ headers, payload });
    offset += totalLength;
  }
  return { events, remaining: buffer.subarray(offset) };
}

function parseHeaders(buffer) {
  const headers = {};
  let offset = 0;
  while (offset < buffer.length) {
    const nameLength = buffer.readUInt8(offset);
    offset += 1;
    const name = buffer.subarray(offset, offset + nameLength).toString("utf8");
    offset += nameLength;
    const type = buffer.readUInt8(offset);
    offset += 1;
    if (type === 7) {
      const len = buffer.readUInt16BE(offset);
      offset += 2;
      headers[name] = buffer.subarray(offset, offset + len).toString("utf8");
      offset += len;
    } else if (type === 0) {
      headers[name] = true;
    } else if (type === 1) {
      headers[name] = false;
    } else if (type === 4) {
      headers[name] = buffer.readInt32BE(offset);
      offset += 4;
    } else {
      throw new Error(`unsupported header type ${type} for ${name}`);
    }
  }
  return headers;
}

function collectRequestSummary(statusCode, bodyText, eventCount, duplicateChunkCount, latencyMs, firstByteMs) {
  inc(state.statuses, String(statusCode));
  state.latencies.push(latencyMs);
  if (firstByteMs != null) state.firstByteLatencies.push(firstByteMs);
  if (statusCode >= 200 && statusCode < 300) {
    state.ok += 1;
  } else {
    state.failed += 1;
  }
  if (duplicateChunkCount > 0) {
    state.duplicateChunks += duplicateChunkCount;
  }
  if (eventCount > 0) {
    const d = digest(bodyText);
    if (state.responseDigests.has(d)) {
      state.duplicateResponses += 1;
    } else {
      state.responseDigests.set(d, 1);
    }
  }
}

function requestOnce(pathname, sequence) {
  const { url, headers, payload } = makeRequest("POST", pathname, requestBodyForSequence(sequence));
  return new Promise((resolve) => {
    const client = url.protocol === "https:" ? https : http;
    const startedAt = performance.now();
    let firstByteAt = null;
    let bodyText = "";
    let eventCount = 0;
    let duplicateChunkCount = 0;
    let previousAssistant = null;
    const req = client.request(
      url,
      { method: "POST", headers, agent },
      (res) => {
        const contentType = String(res.headers["content-type"] || "");
        let buffer = Buffer.alloc(0);
        res.on("data", (chunk) => {
          if (firstByteAt == null) firstByteAt = performance.now();
          buffer = Buffer.concat([buffer, chunk]);
          if (contentType.includes("eventstream")) {
            const decoded = decodeEventStream(buffer);
            buffer = decoded.remaining;
            for (const ev of decoded.events) {
              eventCount += 1;
              try {
                const payload = JSON.parse(ev.payload);
                if (typeof payload.content === "string") {
                  bodyText += payload.content;
                  if (previousAssistant === payload.content) duplicateChunkCount += 1;
                  previousAssistant = payload.content;
                }
              } catch {
                bodyText += ev.payload;
              }
            }
          } else {
            bodyText += chunk.toString("utf8");
          }
        });
        res.on("end", () => {
          const latencyMs = performance.now() - startedAt;
          const firstByteMs = firstByteAt == null ? null : firstByteAt - startedAt;
          collectRequestSummary(res.statusCode || 0, bodyText, eventCount, duplicateChunkCount, latencyMs, firstByteMs);
          resolve({
            statusCode: res.statusCode || 0,
            bodyText,
            latencyMs,
            firstByteMs,
            eventCount,
            duplicateChunkCount,
          });
        });
      }
    );
    req.on("error", (error) => {
      const latencyMs = performance.now() - startedAt;
      state.failed += 1;
      inc(state.errors, error.code || error.message);
      state.latencies.push(latencyMs);
      resolve({ error, latencyMs });
    });
    if (payload) req.write(payload);
    req.end();
  });
}

async function scheduler() {
  const intervalMs = 60_000 / targetRpm;
  const deadline = Date.now() + durationMs;
  let nextAt = Date.now();
  const workers = new Set();
  while (Date.now() < deadline) {
    while (state.inFlight < concurrency && Date.now() >= nextAt && Date.now() < deadline) {
      state.sent += 1;
      state.inFlight += 1;
      const sequence = state.sent;
      const current = requestOnce(path, sequence)
        .catch((error) => {
          state.failed += 1;
          inc(state.errors, error.code || error.message);
        })
        .finally(() => {
          state.inFlight -= 1;
        });
      workers.add(current);
      current.finally(() => workers.delete(current));
      nextAt += intervalMs;
    }
    const waitMs = Math.max(1, Math.min(intervalMs, nextAt - Date.now(), 25));
    await sleep(waitMs);
  }
  while (workers.size > 0) {
    await Promise.race([...workers]);
  }
}

function printSummary() {
  const total = state.ok + state.failed;
  const summary = {
    baseUrl: baseUrl.toString(),
    path,
    scenario,
    streamMode,
    conversationMode,
    durationMs,
    concurrency,
    targetRpm,
    sent: state.sent,
    completed: total,
    ok: state.ok,
    failed: state.failed,
    statusCodes: Object.fromEntries([...state.statuses.entries()].sort(([a], [b]) => a.localeCompare(b, undefined, { numeric: true }))),
    errors: Object.fromEntries([...state.errors.entries()].sort(([a], [b]) => a.localeCompare(b))),
    latencyMs: {
      p50: round(percentile(state.latencies, 50)),
      p95: round(percentile(state.latencies, 95)),
      p99: round(percentile(state.latencies, 99)),
      max: round(Math.max(0, ...state.latencies)),
    },
    firstByteMs: state.firstByteLatencies.length
      ? {
          p50: round(percentile(state.firstByteLatencies, 50)),
          p95: round(percentile(state.firstByteLatencies, 95)),
          p99: round(percentile(state.firstByteLatencies, 99)),
        }
      : null,
    duplicates: {
      responseDigests: state.duplicateResponses,
      adjacentAssistantChunks: state.duplicateChunks,
    },
  };

  if (!noSummary) {
    console.log(JSON.stringify(summary, null, 2));
  } else {
    console.log(JSON.stringify(summary));
  }
}

function round(value) {
  return Math.round(value * 100) / 100;
}

await scheduler();
printSummary();
