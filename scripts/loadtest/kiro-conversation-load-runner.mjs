#!/usr/bin/env node

import http from "node:http";
import https from "node:https";
import { randomUUID } from "node:crypto";
import { setTimeout as sleep } from "node:timers/promises";
import { URL } from "node:url";

const args = parseArgs(process.argv.slice(2));
const baseUrl = new URL(args.baseUrl || process.env.KIRO_BASE_URL || "http://127.0.0.1:9022");
const path = args.path || process.env.KIRO_MESSAGES_PATH || "/cc/v1/messages";
const durationMs = parseDuration(args.duration || process.env.DURATION || "5m");
const concurrency = Number.parseInt(args.concurrency || process.env.CONCURRENCY || "16", 10);
const targetRpm = Number.parseInt(args.rpm || process.env.RPM || "120", 10);
const sessionCount = Number.parseInt(args.sessions || process.env.SESSIONS || "8", 10);
const initialTurns = Number.parseInt(args.initialTurns || process.env.INITIAL_TURNS || "64", 10);
const maxTurns = Number.parseInt(args.maxTurns || process.env.MAX_TURNS || "96", 10);
const toolResultChars = Number.parseInt(args.toolResultChars || process.env.TOOL_RESULT_CHARS || "18000", 10);
const currentUserChars = Number.parseInt(args.currentUserChars || process.env.CURRENT_USER_CHARS || "12000", 10);
const systemChars = Number.parseInt(args.systemChars || process.env.SYSTEM_CHARS || "24000", 10);
const toolDescriptionChars = Number.parseInt(args.toolDescriptionChars || process.env.TOOL_DESCRIPTION_CHARS || "12000", 10);
const streamMode = parseBool(args.stream ?? process.env.STREAM ?? "true");
const scenario = args.scenario || process.env.KIRO_MOCK_SCENARIO || "success";
const apiKey = args.apiKey || process.env.KIRO_API_KEY || "sk-kiro-rs-local-debug";
const noSummary = parseBool(args.noSummary ?? process.env.NO_SUMMARY ?? "false");

const agent =
  baseUrl.protocol === "https:"
    ? new https.Agent({ keepAlive: false, maxSockets: concurrency * 2 })
    : new http.Agent({ keepAlive: false, maxSockets: concurrency * 2 });

const sessions = Array.from({ length: sessionCount }, (_, index) => ({
  id: randomUUID(),
  index,
  turnCount: initialTurns,
  requestCount: 0,
}));

const state = {
  startedAt: Date.now(),
  sent: 0,
  ok: 0,
  failed: 0,
  inFlight: 0,
  statuses: new Map(),
  errors: new Map(),
  latencies: [],
  firstByteLatencies: [],
  payloadBytes: [],
  maxObservedTurns: 0,
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
  const match = String(value).trim().match(/^(\d+(?:\.\d+)?)(ms|s|m|h)?$/i);
  if (!match) throw new Error(`invalid duration: ${value}`);
  const amount = Number.parseFloat(match[1]);
  const unit = (match[2] || "s").toLowerCase();
  if (unit === "ms") return amount;
  if (unit === "s") return amount * 1000;
  if (unit === "m") return amount * 60_000;
  if (unit === "h") return amount * 60 * 60_000;
  throw new Error(`invalid duration unit: ${unit}`);
}

function inc(map, key) {
  map.set(key, (map.get(key) || 0) + 1);
}

function percentile(values, p) {
  if (values.length === 0) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  const idx = Math.min(sorted.length - 1, Math.max(0, Math.ceil((p / 100) * sorted.length) - 1));
  return sorted[idx];
}

function round(value) {
  return Math.round(value * 100) / 100;
}

function repeated(label, targetChars) {
  const chunk = `${label} `.repeat(16);
  return chunk.repeat(Math.ceil(targetChars / chunk.length)).slice(0, targetChars);
}

function toolResultText(session, turn) {
  const header = [
    `session=${session.index}`,
    `turn=${turn}`,
    "command=rg -n payload_guard src && cargo test payload_guard",
    "exit_code=0",
    "stdout:",
  ].join("\n");
  return `${header}\n${repeated(`large historical tool result ${session.index}/${turn}`, toolResultChars)}`;
}

function buildTools() {
  return [
    {
      name: "Bash",
      description: repeated("Bash tool description with long safety and schema notes", toolDescriptionChars),
      input_schema: {
        type: "object",
        properties: {
          command: {
            type: "string",
            description: repeated("command field annotation", 3000),
          },
          description: {
            type: "string",
            description: repeated("description field annotation", 3000),
          },
        },
        required: ["command"],
      },
    },
    {
      name: "Read",
      description: repeated("Read tool description with long file context notes", toolDescriptionChars),
      input_schema: {
        type: "object",
        properties: {
          file_path: {
            type: "string",
            description: repeated("file path annotation", 3000),
          },
          limit: { type: "number" },
        },
        required: ["file_path"],
      },
    },
  ];
}

function buildMessages(session, turns) {
  const messages = [];
  const startTurn = Math.max(0, turns - maxTurns);
  for (let turn = startTurn; turn < turns; turn += 1) {
    const toolUseId = `toolu_mock_${session.index}_${turn}`;
    messages.push({
      role: "user",
      content: `Continue local stress session ${session.index}, historical turn ${turn}. ${repeated("large user prompt", 1200)}`,
    });
    messages.push({
      role: "assistant",
      content: [
        {
          type: "text",
          text: `I will inspect the requested local files for session ${session.index}, turn ${turn}.`,
        },
        {
          type: "tool_use",
          id: toolUseId,
          name: turn % 2 === 0 ? "Bash" : "Read",
          input:
            turn % 2 === 0
              ? { command: `rg -n "payload_guard|UsageRecorder|snapshot" src | head -${20 + (turn % 20)}` }
              : { file_path: "src/anthropic/payload_guard.rs", limit: 220 },
        },
      ],
    });
    messages.push({
      role: "user",
      content: [
        {
          type: "tool_result",
          tool_use_id: toolUseId,
          content: toolResultText(session, turn),
        },
      ],
    });
  }

  messages.push({
    role: "user",
    content: [
      {
        type: "text",
        text: [
          `Current sustained request for session ${session.index}, logical turn ${turns}.`,
          "Keep answering briefly. The local proxy should trim old history and historical tool results before forwarding upstream.",
          repeated("current large user analysis block", currentUserChars),
        ].join("\n\n"),
      },
    ],
  });
  return messages;
}

function buildRequestBody(sequence) {
  const session = sessions[sequence % sessions.length];
  session.requestCount += 1;
  const turns = session.turnCount;
  session.turnCount += 1;
  if (session.turnCount > maxTurns + initialTurns) {
    session.turnCount = maxTurns;
  }
  state.maxObservedTurns = Math.max(state.maxObservedTurns, turns);

  return {
    model: "sonnet",
    max_tokens: 256,
    stream: streamMode,
    metadata: {
      user_id: JSON.stringify({
        device_id: "kiro-sustained-trim-local",
        account_uuid: "mock-e2e",
        session_id: session.id,
        request_sequence: sequence,
      }),
    },
    system: [
      {
        type: "text",
        text: repeated(`stable system prompt for trimming session ${session.index}`, systemChars),
        cache_control: { type: "ephemeral" },
      },
    ],
    tools: buildTools(),
    messages: buildMessages(session, turns),
  };
}

function makeRequest(sequence) {
  const body = buildRequestBody(sequence);
  const payload = Buffer.from(JSON.stringify(body));
  state.payloadBytes.push(payload.length);
  const url = new URL(`${baseUrl.origin}${path}`);
  if (scenario && path === pathname(url)) {
    url.searchParams.set("scenario", scenario);
  }
  const headers = {
    "content-type": "application/json",
    accept: streamMode ? "text/event-stream" : "application/json",
    connection: "close",
    "x-api-key": apiKey,
    "user-agent": "kiro-conversation-loadtest/1.0",
    "content-length": String(payload.length),
  };
  return { url, headers, payload };
}

function pathname(url) {
  return url.pathname;
}

function requestOnce(sequence) {
  const { url, headers, payload } = makeRequest(sequence);
  return new Promise((resolve) => {
    const client = url.protocol === "https:" ? https : http;
    const startedAt = performance.now();
    let firstByteAt = null;
    let settled = false;
    const finishError = (error) => {
      if (settled) return;
      settled = true;
      state.failed += 1;
      inc(state.errors, error.code || error.message);
      state.latencies.push(performance.now() - startedAt);
      resolve();
    };
    const req = client.request(url, { method: "POST", headers, agent }, (res) => {
      res.on("data", () => {
        if (firstByteAt == null) firstByteAt = performance.now();
      });
      res.on("end", () => {
        if (settled) return;
        settled = true;
        const latencyMs = performance.now() - startedAt;
        const firstByteMs = firstByteAt == null ? null : firstByteAt - startedAt;
        inc(state.statuses, String(res.statusCode || 0));
        state.latencies.push(latencyMs);
        if (firstByteMs != null) state.firstByteLatencies.push(firstByteMs);
        if ((res.statusCode || 0) >= 200 && (res.statusCode || 0) < 300) {
          state.ok += 1;
        } else {
          state.failed += 1;
        }
        resolve();
      });
      res.resume();
    });
    req.on("error", finishError);
    req.on("socket", (socket) => {
      socket.once("error", finishError);
    });
    try {
      req.end(payload);
    } catch (error) {
      req.destroy();
      finishError(error);
    }
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
      const current = requestOnce(sequence)
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
    await sleep(Math.max(1, Math.min(intervalMs, nextAt - Date.now(), 25)));
  }
  while (workers.size > 0) {
    await Promise.race([...workers]);
  }
}

function summary() {
  const completed = state.ok + state.failed;
  return {
    baseUrl: baseUrl.toString(),
    path,
    scenario,
    streamMode,
    durationMs,
    concurrency,
    targetRpm,
    sessionCount,
    initialTurns,
    maxTurns,
    toolResultChars,
    currentUserChars,
    systemChars,
    sent: state.sent,
    completed,
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
    payloadBytes: {
      p50: Math.round(percentile(state.payloadBytes, 50)),
      p95: Math.round(percentile(state.payloadBytes, 95)),
      max: Math.max(0, ...state.payloadBytes),
    },
    maxObservedTurns: state.maxObservedTurns,
    sessions: sessions.map((session) => ({
      index: session.index,
      id: session.id,
      requestCount: session.requestCount,
      nextTurnCount: session.turnCount,
    })),
  };
}

await scheduler();
const result = summary();
if (noSummary) {
  console.log(JSON.stringify(result));
} else {
  console.log(JSON.stringify(result, null, 2));
}
