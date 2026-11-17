import { execFileSync } from "node:child_process";

// Liveness comes from a signal-0 probe, not from `ps`/`lsof` succeeding: on a loaded host those
// commands can time out or be killed while the service keeps running, and that must not be
// reported as "service process exited". A failed `ps`/`lsof` only leaves RSS/FD unknown.
export function processAlive(pid) {
  if (!pid) return false;
  try {
    process.kill(Number(pid), 0);
    return true;
  } catch (error) {
    return error?.code === "EPERM";
  }
}

// Classify `lsof -nP -p PID` output. A raw FD total mixes bounded connection pools (the
// upstream reqwest pool keeps idle sockets for 90 s, sqlx keeps idle Postgres connections for
// 10 min) with the descriptors that would actually leak (tokio/kqueue handles, pipes, files,
// client connections never released). The leak judgement therefore looks at kinds, not totals.
//   downstream: TCP sockets on the service's own listen port (clients + the listener)
//   upstream:   TCP sockets whose peer is the mock upstream port
//   db:         any other TCP socket (Postgres, Redis)
//   other:      everything that is not a TCP socket
export function classifyLsof(text, { downstreamPort, upstreamPort } = {}) {
  const kinds = { downstream: 0, upstream: 0, db: 0, other: 0 };
  const lines = text.trim().split("\n").slice(1).filter(Boolean);
  for (const line of lines) {
    const columns = line.trim().split(/\s+/);
    const type = columns[4];
    if (type !== "IPv4" && type !== "IPv6") { kinds.other += 1; continue; }
    // NAME is `local->remote (STATE)` for connections and `local (LISTEN)` for listeners.
    const name = columns.slice(8).join(" ").replace(/\s*\([A-Z_]+\)$/, "");
    const [local, remote] = name.split("->");
    if (remote && upstreamPort && remote.endsWith(`:${upstreamPort}`)) kinds.upstream += 1;
    else if (downstreamPort && local.endsWith(`:${downstreamPort}`)) kinds.downstream += 1;
    else kinds.db += 1;
  }
  return { total: lines.length, ...kinds };
}

export function processSample(pid, ports = {}) {
  if (!pid) return null;
  const alive = processAlive(pid);
  if (!alive) return { rssMb: null, fd: null, fdByKind: null, alive: false };
  let rssMb = null;
  let fd = null;
  let fdByKind = null;
  try {
    const rssKb = Number(execFileSync("ps", ["-o", "rss=", "-p", String(pid)], { encoding: "utf8", timeout: 5000 }).trim());
    if (Number.isFinite(rssKb) && rssKb > 0) rssMb = rssKb / 1024;
  } catch {}
  try {
    const text = execFileSync("lsof", ["-nP", "-p", String(pid)], { encoding: "utf8", timeout: 10000 });
    fdByKind = classifyLsof(text, ports);
    fd = fdByKind.total;
  } catch {}
  return { rssMb, fd, fdByKind, alive: true, sampled: rssMb !== null && fd !== null };
}

// FD leak judgement on classified samples. Returns the list of violated invariants:
//   - non-socket descriptors must be flat (tokio handles, pipes, files);
//   - downstream client sockets must be released once the clients are gone;
//   - upstream sockets can never exceed the slots that were concurrently in flight;
//   - database sockets can never exceed the configured pool capacities.
// Idle pooled sockets below those caps are expected and are not a leak.
export function fdLeakProblems(start, end, { upstreamCap, dbCap } = {}) {
  if (!start?.fdByKind || !end?.fdByKind) return ["FD breakdown unavailable"];
  const problems = [];
  if (end.fdByKind.other > start.fdByKind.other + 10) problems.push(`non-socket FDs grew ${start.fdByKind.other} -> ${end.fdByKind.other}`);
  if (end.fdByKind.downstream > start.fdByKind.downstream + 2) problems.push(`downstream sockets not released ${start.fdByKind.downstream} -> ${end.fdByKind.downstream}`);
  if (upstreamCap && end.fdByKind.upstream > upstreamCap) problems.push(`upstream sockets ${end.fdByKind.upstream} exceed slot cap ${upstreamCap}`);
  if (dbCap && end.fdByKind.db > dbCap) problems.push(`database sockets ${end.fdByKind.db} exceed pool cap ${dbCap}`);
  return problems;
}

export function resourceSummary(samples) {
  const valid = samples.filter((sample) => sample && sample.alive && sample.rssMb !== null && sample.fd !== null);
  if (!valid.length) return null;
  return {
    start: valid[0],
    peak: { rssMb: Math.max(...valid.map((s) => s.rssMb ?? 0)), fd: Math.max(...valid.map((s) => s.fd ?? 0)) },
    end: valid.at(-1),
  };
}
