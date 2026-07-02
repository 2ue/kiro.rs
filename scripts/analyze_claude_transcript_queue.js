#!/usr/bin/env node

const fs = require("node:fs");
const readline = require("node:readline");
const crypto = require("node:crypto");

function usage() {
  console.log(`Usage:
  node scripts/analyze_claude_transcript_queue.js --transcript <path> [--history <path>] [--fail-on-unconsumed] [--no-content]

Purpose:
  Find Claude Code CLI queue-operation inputs that were recorded locally but did not later become a normal user message.
`);
}

function parseArgs(argv) {
  const opts = {
    transcript: null,
    history: null,
    failOnUnconsumed: false,
    showContent: true,
  };

  for (let i = 2; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--transcript") {
      opts.transcript = argv[++i];
    } else if (arg === "--history") {
      opts.history = argv[++i];
    } else if (arg === "--fail-on-unconsumed") {
      opts.failOnUnconsumed = true;
    } else if (arg === "--no-content") {
      opts.showContent = false;
    } else if (arg === "-h" || arg === "--help") {
      usage();
      process.exit(0);
    } else {
      throw new Error(`unknown argument: ${arg}`);
    }
  }

  if (!opts.transcript) {
    usage();
    process.exit(2);
  }
  return opts;
}

function shortHash(text) {
  return crypto.createHash("sha256").update(text).digest("hex").slice(0, 16);
}

function normalizeText(text) {
  return String(text || "").replace(/\s+/g, " ").trim();
}

function previewText(text, maxChars = 160) {
  const normalized = normalizeText(text);
  if (normalized.length <= maxChars) {
    return normalized;
  }
  return `${normalized.slice(0, maxChars)}...`;
}

function matchesQueuedText(candidate, queuedText) {
  const normalizedCandidate = normalizeText(candidate);
  const normalizedQueued = normalizeText(queuedText);
  if (!normalizedCandidate || !normalizedQueued) {
    return false;
  }
  return (
    normalizedCandidate === normalizedQueued ||
    normalizedCandidate.includes(normalizedQueued)
  );
}

function collectNaturalText(value, out, options = {}) {
  const includeToolResults = Boolean(options.includeToolResults);
  if (typeof value === "string") {
    out.push(value);
    return;
  }
  if (Array.isArray(value)) {
    for (const item of value) {
      collectNaturalText(item, out, options);
    }
    return;
  }
  if (!value || typeof value !== "object") {
    return;
  }

  const type = value.type;
  if (type === "tool_result" && !includeToolResults) {
    return;
  }
  if (typeof value.text === "string") {
    out.push(value.text);
    return;
  }
  if (type === "tool_result" && includeToolResults && value.content !== undefined) {
    collectNaturalText(value.content, out, options);
  }
}

function messageNaturalText(message, options = {}) {
  const parts = [];
  collectNaturalText(message && message.content, parts, options);
  return parts.join("\n");
}

function parseJsonLine(line, lineNumber, path) {
  try {
    return JSON.parse(line);
  } catch (error) {
    return {
      type: "__parse_error",
      parseError: error.message,
      lineNumber,
      path,
    };
  }
}

async function readHistoryMatches(historyPath, queueItems) {
  if (!historyPath) {
    return;
  }
  const byText = new Map(queueItems.map((item) => [normalizeText(item.content), item]));
  const stream = fs.createReadStream(historyPath, { encoding: "utf8" });
  const reader = readline.createInterface({ input: stream, crlfDelay: Infinity });
  let lineNumber = 0;

  for await (const line of reader) {
    lineNumber += 1;
    if (!line.trim()) {
      continue;
    }
    const event = parseJsonLine(line, lineNumber, historyPath);
    const display = normalizeText(event.display);
    if (!display) {
      continue;
    }
    const item = byText.get(display);
    if (item) {
      item.historyLines.push(lineNumber);
    }
  }
}

async function analyzeTranscript(transcriptPath) {
  const queueItems = [];
  const parseErrors = [];
  const stream = fs.createReadStream(transcriptPath, { encoding: "utf8" });
  const reader = readline.createInterface({ input: stream, crlfDelay: Infinity });
  let lineNumber = 0;

  for await (const line of reader) {
    lineNumber += 1;
    if (!line.trim()) {
      continue;
    }
    const event = parseJsonLine(line, lineNumber, transcriptPath);
    if (event.type === "__parse_error") {
      parseErrors.push(event);
      continue;
    }

    if (
      event.type === "queue-operation" &&
      event.operation === "enqueue" &&
      typeof event.content === "string" &&
      event.content.trim()
    ) {
      queueItems.push({
        line: lineNumber,
        timestamp: event.timestamp || null,
        sessionId: event.sessionId || null,
        content: event.content,
        hash: shortHash(event.content),
        consumedLine: null,
        consumedTimestamp: null,
        removedLine: null,
        removedTimestamp: null,
        firstAssistantLineAfterEnqueue: null,
        assistantMessagesAfterEnqueue: 0,
        historyLines: [],
      });
      continue;
    }

    if (event.type === "queue-operation" && event.operation === "remove") {
      const pending = queueItems.find(
        (item) => !item.consumedLine && !item.removedLine && item.line < lineNumber,
      );
      if (pending) {
        pending.removedLine = lineNumber;
        pending.removedTimestamp = event.timestamp || null;
      }
      continue;
    }

    if (event.type === "assistant") {
      for (const item of queueItems) {
        if (!item.consumedLine && item.line < lineNumber) {
          item.assistantMessagesAfterEnqueue += 1;
          if (!item.firstAssistantLineAfterEnqueue) {
            item.firstAssistantLineAfterEnqueue = lineNumber;
          }
        }
      }
      continue;
    }

    if (event.type === "user" && event.message && event.message.role === "user") {
      const naturalText = messageNaturalText(event.message, { includeToolResults: false });
      for (const item of queueItems) {
        if (!item.consumedLine && item.line < lineNumber && matchesQueuedText(naturalText, item.content)) {
          item.consumedLine = lineNumber;
          item.consumedTimestamp = event.timestamp || null;
        }
      }
    }
  }

  return { queueItems, parseErrors };
}

function printResult(opts, result) {
  const { queueItems, parseErrors } = result;
  const unconsumed = queueItems.filter((item) => !item.consumedLine);
  const consumed = queueItems.filter((item) => item.consumedLine);

  console.log(`transcript=${opts.transcript}`);
  if (opts.history) {
    console.log(`history=${opts.history}`);
  }
  console.log(`queue_inputs=${queueItems.length}`);
  console.log(`consumed_queue_inputs=${consumed.length}`);
  console.log(`unconsumed_queue_inputs=${unconsumed.length}`);
  if (parseErrors.length > 0) {
    console.log(`parse_errors=${parseErrors.length}`);
  }

  for (const item of consumed) {
    console.log("\n[CONSUMED_QUEUE_INPUT]");
    console.log(`line=${item.line}`);
    console.log(`timestamp=${item.timestamp || ""}`);
    console.log(`hash=${item.hash}`);
    console.log(`consumed_line=${item.consumedLine}`);
    console.log(`consumed_timestamp=${item.consumedTimestamp || ""}`);
    if (opts.showContent) {
      console.log(`content_preview=${previewText(item.content)}`);
    }
  }

  for (const item of unconsumed) {
    console.log("\n[UNCONSUMED_QUEUE_INPUT]");
    console.log(`line=${item.line}`);
    console.log(`timestamp=${item.timestamp || ""}`);
    console.log(`hash=${item.hash}`);
    console.log(`chars=${[...item.content].length}`);
    console.log(`removed_line=${item.removedLine || ""}`);
    console.log(`removed_timestamp=${item.removedTimestamp || ""}`);
    console.log(`first_assistant_line_after_enqueue=${item.firstAssistantLineAfterEnqueue || ""}`);
    console.log(`assistant_messages_after_enqueue=${item.assistantMessagesAfterEnqueue}`);
    console.log(`history_lines=${item.historyLines.join(",")}`);
    console.log("classification=queued_input_not_consumed");
    if (opts.showContent) {
      console.log(`content_preview=${previewText(item.content)}`);
    }
  }
}

async function main() {
  const opts = parseArgs(process.argv);
  const result = await analyzeTranscript(opts.transcript);
  await readHistoryMatches(opts.history, result.queueItems);
  printResult(opts, result);
  if (opts.failOnUnconsumed && result.queueItems.some((item) => !item.consumedLine)) {
    process.exit(1);
  }
}

main().catch((error) => {
  console.error(error && error.stack ? error.stack : String(error));
  process.exit(1);
});
