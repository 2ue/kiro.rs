#!/usr/bin/env node

import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { createInterface } from "node:readline";

const KNOWN_FAILURES_PATH = new URL("./known-test-failures.txt", import.meta.url);
const ANSI_ESCAPE = /\u001b\[[0-?]*[ -/]*[@-~]/g;

export function parseTestOutput(output) {
  const failed = new Set();
  const passed = new Set();
  const runningBinaries = [];
  const completedBinaries = new Set();
  let testResultCount = 0;
  let activeBinaryIndex = null;

  for (const rawLine of output.split(/\r?\n/)) {
    const line = rawLine.replace(ANSI_ESCAPE, "").trim();
    const runningMatch = line.match(/^Running\s+(.+)$/);
    if (runningMatch) {
      runningBinaries.push(runningMatch[1]);
      activeBinaryIndex = runningBinaries.length - 1;
    }
    if (/^Doc-tests\b/.test(line)) {
      activeBinaryIndex = null;
    }
    if (/^test result:\s+/.test(line)) {
      testResultCount += 1;
      if (activeBinaryIndex !== null) {
        completedBinaries.add(activeBinaryIndex);
        activeBinaryIndex = null;
      }
    }

    const failedMatch = line.match(/^test\s+(.+?)\s+\.\.\.\s+FAILED(?:\s|$)/);
    if (failedMatch) {
      failed.add(failedMatch[1]);
      continue;
    }

    const passedMatch = line.match(/^test\s+(.+?)\s+\.\.\.\s+ok(?:\s|$)/);
    if (passedMatch) {
      passed.add(passedMatch[1]);
    }
  }

  return {
    failed,
    passed,
    runningBinaries,
    testResultCount,
    incompleteBinaries: runningBinaries.filter((_, index) => !completedBinaries.has(index)),
    hasTestResult: testResultCount > 0,
  };
}

export function parseKnownFailures(contents) {
  return new Set(
    contents
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter((line) => line.length > 0 && !line.startsWith("#")),
  );
}

export function evaluateTestRun({ output, cargoExitCode, knownFailures }) {
  const parsed = parseTestOutput(output);
  const unexpectedFailures = [...parsed.failed].filter((name) => !knownFailures.has(name));
  const knownFailedTests = [...parsed.failed].filter((name) => knownFailures.has(name));
  const resolvedKnownFailures = [...knownFailures].filter((name) => parsed.passed.has(name));
  const incompleteBinaries = parsed.incompleteBinaries;

  let exitCode = 0;
  if (incompleteBinaries.length > 0) {
    exitCode = cargoExitCode || 1;
  } else if (!parsed.hasTestResult && cargoExitCode !== 0) {
    exitCode = cargoExitCode;
  } else if (unexpectedFailures.length > 0) {
    exitCode = 1;
  } else if (knownFailedTests.length === 0 && cargoExitCode !== 0) {
    exitCode = cargoExitCode;
  }

  return {
    exitCode,
    hasTestResult: parsed.hasTestResult,
    unexpectedFailures,
    knownFailedTests,
    resolvedKnownFailures,
    runningBinaries: parsed.runningBinaries,
    testResultCount: parsed.testResultCount,
    incompleteBinaries,
  };
}

export function formatTestRunReport({ result, cargoExitCode }) {
  const stdout = [];
  const stderr = [];
  const passthroughCargoFailure =
    !result.hasTestResult && cargoExitCode !== 0 && result.incompleteBinaries.length === 0;

  if (!passthroughCargoFailure && result.knownFailedTests.length > 0 && result.exitCode === 0) {
    stdout.push(`仅有已知失败：${result.knownFailedTests.length} 个`);
    stdout.push(...result.knownFailedTests.map((name) => `  ${name}`));
  }
  if (!passthroughCargoFailure && result.unexpectedFailures.length > 0) {
    stderr.push("发现清单之外的失败测试：");
    stderr.push(...result.unexpectedFailures.map((name) => `  ${name}`));
  }
  if (result.incompleteBinaries.length > 0) {
    stderr.push("以下测试二进制缺少 test result: 行：");
    stderr.push(...result.incompleteBinaries.map((name) => `  ${name}`));
  }
  if (!passthroughCargoFailure && result.resolvedKnownFailures.length > 0) {
    stdout.push("以下已知失败测试本次通过，可以从清单中移除：");
    stdout.push(...result.resolvedKnownFailures.map((name) => `  ${name}`));
  }

  return { stdout: stdout.join("\n"), stderr: stderr.join("\n") };
}

async function main() {
  let knownFailures;
  try {
    knownFailures = parseKnownFailures(readFileSync(KNOWN_FAILURES_PATH, "utf8"));
  } catch (error) {
    console.error(`Unable to read known test failures: ${error.message}`);
    process.exit(1);
  }

  const cargoArgs = process.argv.slice(2);
  if (cargoArgs[0] === "--") {
    cargoArgs.shift();
  }

  const cargo = spawn("cargo", ["test", ...cargoArgs], {
    stdio: ["inherit", "pipe", "pipe"],
  });
  const outputLines = [];
  const collectLines = (stream) => {
    const lines = createInterface({ input: stream, crlfDelay: Infinity });
    lines.on("line", (line) => outputLines.push(line));
  };
  collectLines(cargo.stdout);
  collectLines(cargo.stderr);
  cargo.stdout.pipe(process.stdout);
  cargo.stderr.pipe(process.stderr);

  let cargoExitCode = 1;
  try {
    cargoExitCode = await new Promise((resolveExit, reject) => {
      cargo.once("error", reject);
      cargo.once("close", (code) => resolveExit(code ?? 1));
    });
  } catch (error) {
    console.error(`Unable to run cargo test: ${error.message}`);
    process.exit(127);
  }

  const result = evaluateTestRun({
    output: outputLines.join("\n"),
    cargoExitCode,
    knownFailures,
  });
  const report = formatTestRunReport({ result, cargoExitCode });
  if (report.stdout) {
    process.stdout.write(`${report.stdout}\n`);
  }
  if (report.stderr) {
    process.stderr.write(`${report.stderr}\n`);
  }

  process.exit(result.exitCode);
}

const invokedPath = process.argv[1] && new URL(process.argv[1], "file://").href;
if (import.meta.url === invokedPath) {
  await main();
}
