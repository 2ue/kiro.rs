import { execFileSync } from "node:child_process";

export function processSample(pid) {
  if (!pid) return null;
  try {
    const rssKb = Number(execFileSync("ps", ["-o", "rss=", "-p", String(pid)], { encoding: "utf8", timeout: 3000 }).trim());
    const lines = execFileSync("lsof", ["-nP", "-p", String(pid)], { encoding: "utf8", timeout: 3000 }).trim().split("\n");
    return { rssMb: rssKb / 1024, fd: Math.max(0, lines.length - 1), alive: true };
  } catch {
    return { rssMb: null, fd: null, alive: false };
  }
}

export function resourceSummary(samples) {
  const valid = samples.filter(Boolean);
  if (!valid.length) return null;
  return {
    start: valid[0],
    peak: { rssMb: Math.max(...valid.map((s) => s.rssMb ?? 0)), fd: Math.max(...valid.map((s) => s.fd ?? 0)) },
    end: valid.at(-1),
  };
}
