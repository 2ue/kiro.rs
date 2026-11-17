#!/usr/bin/env node

import { spawnSync } from "node:child_process";

// claude-main releases are tagged `claude-vMAJOR.MINOR.PATCH` so they never collide with the
// `v*` tags of the main branch in the same repository.
const tag = process.argv[2] ?? "";
const match = /^claude-v([0-9]+\.[0-9]+\.[0-9]+)$/.exec(tag);
if (!match) {
  console.error(
    `Release tags must use claude-vMAJOR.MINOR.PATCH format; received ${JSON.stringify(tag)}.`,
  );
  process.exit(1);
}

const metadataResult = spawnSync(
  "cargo",
  ["metadata", "--locked", "--no-deps", "--format-version", "1"],
  { encoding: "utf8" },
);
if (metadataResult.status !== 0) {
  process.stderr.write(metadataResult.stderr);
  process.exit(metadataResult.status ?? 1);
}

const metadata = JSON.parse(metadataResult.stdout);
const manifestPath = `${metadata.workspace_root}/Cargo.toml`;
const packageMetadata = metadata.packages.find(
  (candidate) => candidate.manifest_path === manifestPath,
);
if (!packageMetadata) {
  console.error(`Cargo metadata did not contain the root package at ${manifestPath}.`);
  process.exit(1);
}

const taggedVersion = match[1];
if (taggedVersion !== packageMetadata.version) {
  console.error(
    `Release tag ${tag} does not match Cargo package version ${packageMetadata.version}.`,
  );
  process.exit(1);
}

console.log(`Release tag ${tag} matches Cargo package version ${packageMetadata.version}.`);
