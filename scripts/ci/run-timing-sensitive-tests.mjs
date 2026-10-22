#!/usr/bin/env node
// Runs the tests marked `#[ignore = "timing-sensitive: ..."]` alone and single-threaded.
// They assert wall-clock budgets, so the parallel CI run skips them (see build.yaml).

import { spawnSync } from "node:child_process";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

const MARKER = /#\[ignore = "timing-sensitive:[^\n]*\n(?:[ \t]*#\[[^\n]*\n)*[ \t]*(?:async )?fn ([a-z0-9_]+)\(/g;

export function findTimingSensitiveTests(root = "src") {
  const names = [];
  const walk = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) walk(path);
      else if (path.endsWith(".rs")) {
        for (const match of readFileSync(path, "utf8").matchAll(MARKER)) names.push(match[1]);
      }
    }
  };
  walk(root);
  return [...new Set(names)].sort();
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const names = findTimingSensitiveTests();
  if (names.length === 0) {
    console.log("No timing-sensitive tests found.");
    process.exit(0);
  }
  console.log(`Running ${names.length} timing-sensitive tests:\n  ${names.join("\n  ")}`);
  const result = spawnSync(
    "cargo",
    ["test", "--locked", "--bin", "kiro-rs", "--", "--ignored", "--test-threads=1", ...names],
    { stdio: "inherit" },
  );
  process.exit(result.status ?? 1);
}
