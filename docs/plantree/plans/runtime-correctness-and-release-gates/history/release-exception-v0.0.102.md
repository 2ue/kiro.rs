# Release Exception: v0.0.102

Role: Dated release-history exception

Status: Closed, version-specific exception

Date: 2026-07-11

Authority: Records what was intentionally skipped for `v0.0.102`; it does not redefine normal release gates

Related: [Evidence index](evidence-index.md), [Runtime plan](../README.md), [Current test and release gates](../../../baseline/test-and-release-gates.md)

## Scope

The operator explicitly instructed the release action to:

- generate the requested Excel evidence first;
- update the project version;
- create the release commit and tag;
- push the release;
- skip local compilation verification for this release action only.

The resulting repository release commit is:

- version: `0.0.102`;
- tag: `v0.0.102`;
- commit: `e9479df71ee0044cfa0da8acbf69d98c2259a66f`;
- commit date: 2026-07-11;
- subject: `chore(release): 0.0.102`.

Remote verification on 2026-07-11 showed `origin/main` and the dereferenced `refs/tags/v0.0.102^{}` both resolve to `e9479df71ee0044cfa0da8acbf69d98c2259a66f`. The annotated tag object itself has its own tag-object hash, as expected.

## What This Exception Means

- No new local compilation/build result was required before the version/tag/push action.
- The release could rely on already existing evidence only to the extent that evidence actually covered the relevant source.
- The release action intentionally prioritized the explicit publication instruction over the ordinary local pre-release build sequence.

## What This Exception Does Not Mean

- It does not prove the end-to-end Docker image gate passed.
- It does not reinterpret the Docker timeout during `cargo fetch --locked` as a compilation failure or success.
- It does not waive protocol, storage, frontend, load, security, or supply-chain gates for future versions.
- It does not change the deferred `6.P1` scope.
- It does not authorize another release to skip verification merely because `v0.0.102` did.
- It does not associate the earlier historical release-binary hashes with the `v0.0.102` tag; the historical evidence did not record its exact source revision, and no local binary was rebuilt for the version-only release commit.

## Excel Evidence Gap

The release instruction referenced generating an Excel workbook before version/tag/push, but no versioned evidence manifest records the requested cache-test workbook's filename, path, SHA-256, sheets, row counts, fields, or cleanup disposition.

A 2026-07-11 project-only audit found one local workbook:

- path: `tmp/usage_2026-07-02_to_2026-07-03.xlsx`;
- size: 49,294 bytes;
- modified: 2026-07-03 03:53:53 +0800;
- SHA-256: `57b7e5c8fec5e821d88cbfc0516af2e6170a312da3cb9c123b4cfa66388e588e`.

No durable source proves that this older usage workbook is the cache-test export referenced by the later release instruction. It must not be presented as that deliverable without an independent manifest or operator confirmation. The workbook was not modified or deleted by this documentation audit.

## Future Rule

Every later release follows the checked-in normal gates unless the operator grants another exception that names the exact version, skipped gates, reason, consequences, and expiry. A version-specific exception must be recorded before the release is later described as fully verified.
