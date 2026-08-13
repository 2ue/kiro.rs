#!/usr/bin/env node

import { spawn, spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { relative, resolve, sep } from "node:path";

const EXPECTED_RUSTC_VERSION = "1.92.0";
const BASELINE_PATH = resolve("scripts/ci/clippy-baseline.json");
const CLIPPY_ARGS = [
  "clippy",
  "--locked",
  "--all-targets",
  "--all-features",
  "--message-format=json",
];
const update = process.argv.includes("--update");

const rustcResult = spawnSync("rustc", ["--version"], { encoding: "utf8" });
if (rustcResult.status !== 0) {
  process.stderr.write(rustcResult.stderr);
  process.exit(rustcResult.status ?? 1);
}
const rustcVersion = rustcResult.stdout.trim().split(/\s+/)[1];
if (rustcVersion !== EXPECTED_RUSTC_VERSION) {
  console.error(
    `Clippy baseline requires rustc ${EXPECTED_RUSTC_VERSION}; found ${rustcVersion}.`,
  );
  process.exit(1);
}

const warnings = new Map();
let stdoutBuffer = "";
const cargo = spawn("cargo", CLIPPY_ARGS, {
  env: { ...process.env, CARGO_TERM_COLOR: "never" },
  stdio: ["inherit", "pipe", "pipe"],
});

cargo.stderr.pipe(process.stderr);
cargo.stdout.setEncoding("utf8");
cargo.stdout.on("data", (chunk) => {
  stdoutBuffer += chunk;
  const lines = stdoutBuffer.split("\n");
  stdoutBuffer = lines.pop() ?? "";
  for (const line of lines) {
    recordWarning(line, warnings);
  }
});

const exitCode = await new Promise((resolveExit) => {
  cargo.on("error", (error) => {
    console.error(`Unable to run cargo clippy: ${error.message}`);
    resolveExit(1);
  });
  cargo.on("close", (code) => resolveExit(code ?? 1));
});
if (stdoutBuffer) {
  recordWarning(stdoutBuffer, warnings);
}
if (exitCode !== 0) {
  process.exit(exitCode);
}

const sortedLimits = Object.fromEntries(
  [...warnings.entries()].sort(([left], [right]) => left.localeCompare(right)),
);
const totalWarnings = sumCounts(sortedLimits);

if (update) {
  const baseline = {
    schemaVersion: 1,
    rustcVersion: EXPECTED_RUSTC_VERSION,
    cargoArgs: CLIPPY_ARGS,
    limits: sortedLimits,
  };
  writeFileSync(BASELINE_PATH, `${JSON.stringify(baseline, null, 2)}\n`);
  console.log(
    `Updated Clippy baseline with ${totalWarnings} warnings in ${warnings.size} lint/file buckets.`,
  );
  process.exit(0);
}

const baseline = JSON.parse(readFileSync(BASELINE_PATH, "utf8"));
if (
  baseline.schemaVersion !== 1 ||
  baseline.rustcVersion !== EXPECTED_RUSTC_VERSION ||
  JSON.stringify(baseline.cargoArgs) !== JSON.stringify(CLIPPY_ARGS)
) {
  console.error(
    "Clippy baseline metadata does not match the pinned toolchain and command. Regenerate it intentionally with --update.",
  );
  process.exit(1);
}

const regressions = [];
for (const [key, count] of warnings) {
  const allowed = baseline.limits[key] ?? 0;
  if (count > allowed) {
    regressions.push({ key, count, allowed });
  }
}

const baselineTotal = sumCounts(baseline.limits);
console.log(
  `Clippy emitted ${totalWarnings} warnings; the checked-in baseline allows ${baselineTotal}.`,
);
if (totalWarnings < baselineTotal) {
  console.log(
    "The warning count decreased. Regenerate the baseline with --update in a dedicated lint-cleanup change to ratchet it down.",
  );
}
if (regressions.length === 0) {
  process.exit(0);
}

console.error("Clippy warning baseline exceeded:");
for (const { key, count, allowed } of regressions.sort((a, b) => a.key.localeCompare(b.key))) {
  console.error(`  ${key}: ${count} warning(s), baseline ${allowed}`);
}
console.error(
  "Fix the new warnings. Only use --update when deliberately accepting or reducing the repository baseline.",
);
process.exit(1);

function recordWarning(line, counts) {
  let event;
  try {
    event = JSON.parse(line);
  } catch {
    return;
  }
  if (event.reason !== "compiler-message" || !event.package_id?.includes("#kiro-rs@")) {
    return;
  }
  if (event.message?.level !== "warning") {
    if (event.message?.level === "error" && event.message.rendered) {
      process.stderr.write(event.message.rendered);
    }
    return;
  }

  const lint = event.message.code?.code ?? "uncoded-warning";
  const primarySpan = event.message.spans?.find((span) => span.is_primary);
  const file = normalizePath(primarySpan?.file_name ?? "<crate>");
  const key = `${lint} | ${file}`;
  counts.set(key, (counts.get(key) ?? 0) + 1);
}

function normalizePath(file) {
  if (file === "<crate>") {
    return file;
  }
  const absolute = resolve(file);
  const projectRelative = relative(process.cwd(), absolute);
  return projectRelative.split(sep).join("/");
}

function sumCounts(limits) {
  return Object.values(limits).reduce((total, count) => total + count, 0);
}
