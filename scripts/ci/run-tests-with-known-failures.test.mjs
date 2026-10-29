import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";

import {
  evaluateTestRun,
  formatTestRunReport,
  githubFailureAnnotations,
  parseKnownFailures,
} from "./run-tests-with-known-failures.mjs";

const knownFailure = "module::tests::known_failure";
const knownFailures = parseKnownFailures(`# persistent failures\n\n${knownFailure}\n`);

test("known failures are reported but do not fail the test command", () => {
  const result = evaluateTestRun({
    output: `test ${knownFailure} ... FAILED\ntest result: FAILED. 0 passed; 1 failed;`,
    cargoExitCode: 101,
    knownFailures,
  });
  const report = formatTestRunReport({ result, cargoExitCode: 101 });

  assert.equal(result.exitCode, 0);
  assert.deepEqual(result.knownFailedTests, [knownFailure]);
  assert.deepEqual(result.unexpectedFailures, []);
  assert.equal(report.stdout, `仅有已知失败：1 个\n  ${knownFailure}`);
  assert.equal(report.stderr, "");
});

test("failures outside the known list fail the test command", () => {
  const result = evaluateTestRun({
    output: "test module::tests::new_failure ... FAILED\ntest result: FAILED. 0 passed; 1 failed;",
    cargoExitCode: 101,
    knownFailures,
  });
  const report = formatTestRunReport({ result, cargoExitCode: 101 });

  assert.equal(result.exitCode, 1);
  assert.deepEqual(result.unexpectedFailures, ["module::tests::new_failure"]);
  assert.equal(report.stderr, "发现清单之外的失败测试：\n  module::tests::new_failure");
});

test("known tests that pass are suggested for removal", () => {
  const result = evaluateTestRun({
    output: `test ${knownFailure} ... ok\ntest result: ok. 1 passed; 0 failed;`,
    cargoExitCode: 0,
    knownFailures,
  });
  const report = formatTestRunReport({ result, cargoExitCode: 0 });

  assert.equal(result.exitCode, 0);
  assert.deepEqual(result.resolvedKnownFailures, [knownFailure]);
  assert.equal(
    report.stdout,
    `以下已知失败测试本次通过，可以从清单中移除：\n  ${knownFailure}`,
  );
});

test("cargo failures without a test result keep the cargo exit code", () => {
  const result = evaluateTestRun({
    output: "error: could not compile `kiro-rs` due to previous error",
    cargoExitCode: 101,
    knownFailures,
  });
  const report = formatTestRunReport({ result, cargoExitCode: 101 });

  assert.equal(result.hasTestResult, false);
  assert.equal(result.exitCode, 101);
  assert.deepEqual(report, { stdout: "", stderr: "" });
});

test("a missing test result for a later binary fails after known failures", () => {
  const laterBinary = "unittests src/bin/kiro_loadtest.rs (target/debug/deps/kiro_loadtest-deadbeef)";
  const result = evaluateTestRun({
    output: [
      "Running unittests src/main.rs (target/debug/deps/kiro_rs-deadbeef)",
      `test ${knownFailure} ... FAILED`,
      "test result: FAILED. 0 passed; 1 failed;",
      `Running ${laterBinary}`,
    ].join("\n"),
    cargoExitCode: 101,
    knownFailures,
  });
  const report = formatTestRunReport({ result, cargoExitCode: 101 });

  assert.equal(result.runningBinaries.length, 2);
  assert.equal(result.testResultCount, 1);
  assert.ok(result.testResultCount < result.runningBinaries.length);
  assert.deepEqual(result.incompleteBinaries, [laterBinary]);
  assert.notEqual(result.exitCode, 0);
  assert.equal(
    report.stderr,
    `以下测试二进制缺少 test result: 行：\n  ${laterBinary}`,
  );
});

test("zero-test targets and doc-tests still count as completed test results", () => {
  const result = evaluateTestRun({
    output: [
      "Running unittests src/lib.rs (target/debug/deps/kiro_rs-deadbeef)",
      "test result: ok. 0 passed; 0 failed; 0 ignored;",
      "Doc-tests kiro_rs",
      "test result: ok. 0 passed; 0 failed; 0 ignored;",
    ].join("\n"),
    cargoExitCode: 0,
    knownFailures,
  });

  assert.equal(result.runningBinaries.length, 1);
  assert.equal(result.testResultCount, 2);
  assert.deepEqual(result.incompleteBinaries, []);
  assert.equal(result.exitCode, 0);
});

test("doc-test output cannot hide a missing test result for a binary", () => {
  const laterBinary = "unittests src/bin/kiro_loadtest.rs (target/debug/deps/kiro_loadtest-deadbeef)";
  const result = evaluateTestRun({
    output: [
      "Running unittests src/main.rs (target/debug/deps/kiro_rs-deadbeef)",
      `test ${knownFailure} ... FAILED`,
      "test result: FAILED. 0 passed; 1 failed;",
      `Running ${laterBinary}`,
      "Doc-tests kiro_rs",
      "test result: ok. 0 passed; 0 failed; 0 ignored;",
    ].join("\n"),
    cargoExitCode: 101,
    knownFailures,
  });

  assert.equal(result.runningBinaries.length, result.testResultCount);
  assert.deepEqual(result.incompleteBinaries, [laterBinary]);
  assert.notEqual(result.exitCode, 0);
});

test("known failure list holds only fully qualified test names", () => {
  const actual = parseKnownFailures(
    readFileSync(new URL("./known-test-failures.txt", import.meta.url), "utf8"),
  );

  for (const name of actual) {
    assert.match(name, /^[A-Za-z_][A-Za-z0-9_]*(::[A-Za-z_][A-Za-z0-9_]*)+$/);
  }
});

test("Quality Gate runs the full profile on main pushes through the filter", () => {
  const workflow = readFileSync(new URL("../../.github/workflows/build.yaml", import.meta.url), "utf8");

  assert.match(workflow, /push:\n\s+branches:\n\s+- main/);
  assert.match(workflow, /quality-\$\{\{ github\.workflow \}\}-\$\{\{ github\.event\.pull_request\.number \|\| github\.ref \}\}/);
  assert.match(workflow, /QUALITY_PROFILE: \$\{\{ inputs\.profile \|\| 'full' \}\}/);
  // Both feature sets run in parallel matrix jobs, each multi-threaded.
  assert.match(workflow, /cargo_args: ''\n\s+- name: no default features\n\s+cargo_args: --no-default-features/);
  assert.match(
    workflow,
    /node scripts\/ci\/run-tests-with-known-failures\.mjs -- --no-fail-fast --locked --all-targets \$\{\{ matrix\.cargo_args \}\}/,
  );
  assert.doesNotMatch(workflow, /run-tests-with-known-failures\.mjs[^\n]*--test-threads=1/);
});

test("manifest checks a successful same-SHA push and exposes the documented escape hatch", () => {
  const workflow = readFileSync(
    new URL("../../.github/workflows/docker-build.yaml", import.meta.url),
    "utf8",
  );
  const gateIndex = workflow.indexOf("- name: Verify successful full Quality Gate");
  const manifestIndex = workflow.indexOf("- name: Create and push multi-arch manifests");
  const manifestSection = workflow.slice(workflow.indexOf("  manifest:"));

  assert.ok(gateIndex >= 0 && gateIndex < manifestIndex);
  assert.match(
    manifestSection,
    /permissions:\n\s+actions: read\n\s+contents: read\n\s+packages: write/,
  );
  // The manifest waits for the same-SHA push run to finish, then requires it to have succeeded.
  assert.match(workflow, /workflow_id: "build\.yaml",\n\s+head_sha: context\.sha,/);
  assert.match(workflow, /run\.head_sha === context\.sha/);
  assert.match(workflow, /run\.event === "push"/);
  assert.match(workflow, /run\.conclusion === "success"/);
  assert.match(workflow, /workflow_dispatch run exists[\s\S]*?Manually confirm it used profile=full/);
  assert.match(workflow, /vars\.ALLOW_RELEASE_WITHOUT_FULL_QUALITY != 'true'/);
  assert.match(workflow, /vars\.ALLOW_RELEASE_WITHOUT_FULL_QUALITY == 'true'/);
  assert.match(workflow, /Please wait for the Quality Gate run on main for this commit to finish and pass, or manually dispatch the full profile/);
  // octokit 的方法名是 listWorkflowRuns（GET /actions/workflows/{id}/runs）；写错方法名只会在发版时才报
  // "is not a function"，所以在这里锁住。
  assert.match(workflow, /github\.rest\.actions\.listWorkflowRuns\(/);
  assert.doesNotMatch(workflow, /listWorkflowRunsForWorkflow/);
});

test("unexpected failures become GitHub error annotations with the panic location", () => {
  const output = [
    "test a::tests::broken ... FAILED",
    "test b::tests::fine ... ok",
    "failures:",
    "",
    "---- a::tests::broken stdout ----",
    "",
    "thread 'a::tests::broken' (1234) panicked at src/a/tests.rs:42:9:",
    "assertion `left == right` failed: round=1",
    "  left: 1",
    " right: 2",
    "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace",
    "test result: FAILED. 1 passed; 1 failed;",
  ].join("\n");
  const result = evaluateTestRun({ output, cargoExitCode: 101, knownFailures });

  assert.deepEqual(githubFailureAnnotations(result, output, {}), []);
  const [annotation] = githubFailureAnnotations(result, output, { GITHUB_ACTIONS: "true" });
  assert.match(annotation, /^::error title=test failed%3A a%3A%3Atests%3A%3Abroken,file=src\/a\/tests.rs,line=42::/);
  assert.match(annotation, /assertion `left == right` failed: round=1/);
  assert.match(annotation, /left: 1/);
});

test("a failure without a panic block still gets an annotation naming the test", () => {
  const output = "test a::tests::hung ... FAILED\ntest result: FAILED. 0 passed; 1 failed;";
  const result = evaluateTestRun({ output, cargoExitCode: 101, knownFailures });
  const [annotation] = githubFailureAnnotations(result, output, { GITHUB_ACTIONS: "true" });
  assert.equal(annotation, "::error title=test failed%3A a%3A%3Atests%3A%3Ahung::a::tests::hung");
});
