#!/usr/bin/env python3
"""Check overload-probe results against the two invariants.

A: while requests wait (queued or rejected), the upstream slots are used, i.e. capacity that is
   there is dispatched. Measured as the average slot use over waiting seconds; for pools with a
   per-account RPM, as the share of slots that both concurrency and RPM would still allow.
B: the service stays up and recovers: no stalled seconds while requests wait, /readyz answers,
   leases go back to zero, and after the load drops the queue drains.

Usage: overload-verdict.py RESULT_DIR [--min-slot-use 0.8] [--max-free 0.1]
"""
import glob
import json
import os
import sys

result_dir = sys.argv[1]
min_slot_use = float(sys.argv[sys.argv.index("--min-slot-use") + 1]) if "--min-slot-use" in sys.argv else 0.8
# RPM-limited pools: the share of the pool's slots still free to dispatch (concurrency and RPM
# window both allowing) while requests wait. Slots free up between samples, so 0 is not the bar.
max_free = float(sys.argv[sys.argv.index("--max-free") + 1]) if "--max-free" in sys.argv else 0.1
# Fewer waiting seconds than this in a phase is a blip between a release and the next dispatch.
MIN_WAITING_SECS = 5

# Cases where upstream itself refuses or is down during a phase: slot use is not expected to be
# high then (there is no capacity to dispatch to), only recovery afterwards is checked.
UPSTREAM_DOWN_PHASES = {
    "storm_5xx": {1},
    "storm_429": {1},
    "storm_429_noretry": {1},
    "storm_reset": {1},
    "storm_midstream": {1},
    "rpm_storm_429": {1},
    "pool_5xx_recover": {1},
    "pool_queue_fault": {1},
}

failures = 0
rows = []
for path in sorted(glob.glob(os.path.join(result_dir, "*.json"))):
    name = os.path.basename(path)[:-5]
    report = json.load(open(path))
    problems = []
    if report.get("abortedReason"):
        problems.append(f"aborted: {report['abortedReason']}")
    if report.get("accountsOverConcurrency"):
        problems.append(f"{report['accountsOverConcurrency']} accounts over their concurrency")
    if report.get("accountsOverRpm"):
        problems.append(f"{report['accountsOverRpm']} accounts over their RPM {report.get('accountsOverRpmDetail')}")
    if report.get("unfinishedArrivals"):
        problems.append(f"{report['unfinishedArrivals']} requests still open after the drain")
    phases = report["phases"]
    capacity = report.get("capacity") or report["accounts"] * report["perConcurrency"]
    # Older reports have no rpmLimited field; their RPM case was the only one with a per-account RPM.
    rpm_limited = report.get("rpmLimited", bool(report.get("perRpm")))
    timeline = report["timeline"]
    segment_ends = []
    acc = 0
    for phase in phases[:-1]:
        acc += phase["secs"]
        segment_ends.append(acc)

    def phase_index(t):
        for index, end in enumerate(segment_ends):
            if t < end:
                return index
        return len(phases) - 1

    # Stalled second: requests queued, no upstream call started for 5 s, and the upstream slots
    # are not full (full slots with slow responses are busy, not stalled). With an RPM limit the
    # pool may also be waiting for its windows: only seconds with room left in some account count.
    stalled = [0] * len(phases)
    for position, sample in enumerate(timeline):
        recent = timeline[max(0, position - 4) : position + 1]
        if (
            len(recent) == 5
            and (sample["queued"] or 0) > 0
            and sum(item["upstreamDone"] for item in recent) == 0
            and sample["upstreamInFlight"] < 0.9 * capacity
            and (not rpm_limited or sample.get("freeUnits", 0) > 0)
        ):
            stalled[phase_index(sample["t"])] += 1
    for index, phase in enumerate(phases):
        label = phase["segment"]
        down = index in UPSTREAM_DOWN_PHASES.get(name, set())
        if stalled[index] > 2 and not down:
            problems.append(f"[{label}] stalled {stalled[index]} s")
        if phase["notReadySecs"] > 2:
            problems.append(f"[{label}] /readyz not ready {phase['notReadySecs']} s")
        use = phase.get("slotUseWhileWaiting")
        waiting_secs = sum(
            1
            for sample in timeline
            if phase_index(sample["t"]) == index and ((sample["queued"] or 0) > 0 or sample["rejected"] > 0)
        )
        free = phase.get("freeWhileWaiting")
        if waiting_secs >= MIN_WAITING_SECS and not down:
            if rpm_limited:
                if free is not None and free > max_free:
                    problems.append(f"[{label}] free slots while waiting {free:.2f} > {max_free}")
            elif use is not None and use < min_slot_use:
                problems.append(f"[{label}] slot use while waiting {use:.2f} < {min_slot_use}")
    drain = timeline[-5:]
    if any((sample["queued"] or 0) > 0 for sample in drain):
        problems.append("queue still not empty at the end")
    verdict = "PASS" if not problems else "FAIL"
    failures += bool(problems)
    ok = sum(phase["ok"] for phase in phases)
    rejected = sum(phase["rejected"] for phase in phases)
    rows.append((name, verdict, ok, rejected, problems))

for name, verdict, ok, rejected, problems in rows:
    print(f"{verdict}  {name:22s} ok={ok:<6} 429={rejected:<7} {'; '.join(problems)}")
print(f"\n{len(rows) - failures}/{len(rows)} passed")
sys.exit(1 if failures else 0)
