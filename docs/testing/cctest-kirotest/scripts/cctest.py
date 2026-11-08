#!/usr/bin/env python3
"""CCTest CLI for kiro.rs route testing.

Secrets come from env or the repo-local `.env.local` (git-ignored):
  CCTEST_API_KEY         cctest.ai key (cct-...)
  CCTEST_TARGET_BASE     domain base URL of kiro.rs, e.g. https://kiro2.okmcode.com
  CCTEST_TARGET_API_KEY  kiro.rs request API key handed to cctest as the upstream key

Usage:
  cctest.py check --route /dfcache/mine-haa [--model claude-opus-4-8] [--no-token-usage]
                  [--concurrency 1] [--attribution-only] [--label NAME] [--repeat N]
  cctest.py get TASK_ID
  cctest.py status [MONITOR_ID]
  cctest.py summary FILE...        # re-print summaries of saved results

Results are saved under docs/testing/cctest-kirotest/results/ (the result JSON is reviewable; secrets stay in the environment).
"""
import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parents[4]
API = "https://cctest.ai"
OUT_DIR = REPO / "docs" / "testing" / "cctest-kirotest" / "results"


def load_env():
    for name in (".env.local", ".env"):
        path = REPO / name
        if not path.exists():
            continue
        for line in path.read_text().splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, value = line.split("=", 1)
            os.environ.setdefault(key.strip(), value.strip().strip('"').strip("'"))


def need(name):
    value = os.environ.get(name, "")
    if not value:
        sys.exit(f"missing {name}; set it in {REPO / '.env.local'}")
    return value


def call(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        API + path,
        data=data,
        method=method,
        headers={
            "Authorization": f"Bearer {need('CCTEST_API_KEY')}",
            "Content-Type": "application/json",
            "User-Agent": "kiro-rs-cctest-skill/1.0",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as err:
        sys.exit(f"HTTP {err.code} {path}: {err.read().decode()[:500]}")


def poll(task_id, interval=3.0, timeout=1800):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        res = call("GET", f"/api/v1/check/{task_id}")
        state = (res.get("status"), res.get("stepName"), res.get("progress"))
        if state != last:
            print(f"  [{task_id[:8]}] status={state[0]} step={state[1]} progress={state[2]}", file=sys.stderr)
            last = state
        if res.get("status") in ("done", "error"):
            return res
        time.sleep(interval)
    sys.exit(f"timeout waiting for {task_id}")


def summarize(res, label=""):
    audit = res.get("tokenAudit") or {}
    metrics = res.get("metrics") or {}
    return {
        "label": label or res.get("_label", ""),
        "status": res.get("status"),
        "verdict": res.get("verdictKey"),
        "total": res.get("total"),
        "responseModel": res.get("responseModel"),
        "ratio": audit.get("overallRatio"),
        "cacheHitRate": audit.get("cacheHitRate"),
        "totalCost": audit.get("totalCost"),
        "baselineCost": audit.get("baselineTotalCost"),
        "anomalies": audit.get("anomalies"),
        "ttfbMs": metrics.get("ttfbMs"),
        "tps": metrics.get("tokensPerSec"),
        "error": res.get("error"),
        "kiro": res.get("_kiro"),
    }


def kiro_stats(started, finished, route=None):
    """Aggregate kiro.rs usage_records for this run (matched by request key hash + time window).

    Needs CCTEST_KIRO_PSQL, e.g.
    "docker exec -i kiro-rs-2ue-59138-postgres psql -U kiro_rs_59137 -d kiro_rs_59137".
    """
    psql = os.environ.get("CCTEST_KIRO_PSQL")
    if not psql:
        return None
    import hashlib
    import shlex
    import subprocess

    key_hash = hashlib.sha256(need("CCTEST_TARGET_API_KEY").encode()).hexdigest()
    # Filtering by endpoint keeps concurrent runs on different routes apart.
    endpoint_filter = ""
    if route is not None:
        endpoint = (route.rstrip("/") + "/v1/messages").replace("'", "")
        endpoint_filter = f"and endpoint = '{endpoint}'"
    sql = f"""
select json_build_object(
  'requests', count(*),
  'local', count(*) filter (where credential_id is not null),
  'rawInput', coalesce(sum((data->'rawUsage'->>'inputTokens')::bigint),0),
  'billableInput', coalesce(sum(billable_input_tokens),0),
  'compatInput', coalesce(sum(compat_input_tokens),0),
  'cacheRead', coalesce(sum(cache_read_input_tokens),0),
  'cacheCreation', coalesce(sum(cache_creation_input_tokens),0),
  'output', coalesce(sum(output_tokens),0),
  'usd', coalesce(sum(estimated_cost_usd),0),
  'originalUsd', coalesce(sum(original_cost_usd),0),
  'credits', coalesce(sum(kiro_metering_usage),0))
from usage_records
where deleted_at is null and data->>'requestApiKeyId' = '{key_hash}'
  and created_at >= '{started.isoformat()}' and created_at <= '{finished.isoformat()}'
  {endpoint_filter}"""
    out = subprocess.run(shlex.split(psql) + ["-At", "-c", sql], capture_output=True, text=True)
    if out.returncode != 0:
        return {"error": out.stderr.strip()[:200]}
    s = json.loads(out.stdout.strip())
    # Same formula as the kiro.rs overview: uncached input = compat_input_tokens.
    denom = s["compatInput"] + s["cacheRead"] + s["cacheCreation"]
    s["cacheHitRate"] = round(s["cacheRead"] / denom * 100, 1) if denom else None
    s["usdPerCredit"] = round(s["usd"] / s["credits"], 3) if s["credits"] else None
    return s


def cmd_check(args):
    base = (args.target_base or "https://kirotest.okmcode.com").rstrip("/")
    if urllib.parse.urlsplit(base).hostname != "kirotest.okmcode.com":
        sys.exit("refusing non-local test target; expected kirotest.okmcode.com")
    route = (args.route or "").rstrip("/")
    url = base + route
    body = {
        "url": url,
        "apiKey": need("CCTEST_TARGET_API_KEY"),
        "model": args.model,
        "checkTokenUsage": not args.no_token_usage,
        "concurrency": args.concurrency,
    }
    if args.attribution_only:
        body["attributionOnly"] = True
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    label = args.label or (route.strip("/").replace("/", "_") or "root")
    rows = []
    for i in range(args.repeat):
        started = datetime.now(timezone.utc)
        task = call("POST", "/api/v1/check", body)
        task_id = task.get("taskId")
        if not task_id:
            sys.exit(f"no taskId: {task}")
        print(f"[{i + 1}/{args.repeat}] {url} model={args.model} task={task_id}", file=sys.stderr)
        res = poll(task_id)
        res["_label"] = label
        res["_url"] = url
        res["_route"] = route
        res["_model"] = args.model
        res["_startedAt"] = started.isoformat()
        finished = datetime.now(timezone.utc)
        res["_finishedAt"] = finished.isoformat()
        time.sleep(3)  # usage records are written asynchronously
        res["_kiro"] = kiro_stats(started, finished, route)
        out = OUT_DIR / f"{started.strftime('%Y%m%dT%H%M%S')}-{label}.json"
        out.write_text(json.dumps(res, ensure_ascii=False, indent=2))
        row = summarize(res, label)
        row["file"] = str(out.relative_to(REPO))
        rows.append(row)
        print(json.dumps(row, ensure_ascii=False))
    return rows


def main():
    load_env()
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("check")
    c.add_argument("--route", default="", help="route prefix such as /dfcache/mine-haa ('' = root)")
    c.add_argument("--target-base", default="", help="explicit target base URL; overrides CCTEST_TARGET_BASE")
    c.add_argument("--model", default="claude-opus-4-8")
    c.add_argument("--no-token-usage", action="store_true")
    c.add_argument("--attribution-only", action="store_true")
    c.add_argument("--concurrency", type=int, default=1)
    c.add_argument("--label", default="")
    c.add_argument("--repeat", type=int, default=1)
    g = sub.add_parser("get")
    g.add_argument("task_id")
    s = sub.add_parser("status")
    s.add_argument("monitor_id", nargs="?")
    m = sub.add_parser("summary")
    m.add_argument("files", nargs="+")
    m.add_argument("--recompute", action="store_true", help="re-query kiro.rs usage for each saved run")
    args = parser.parse_args()

    if args.cmd == "check":
        cmd_check(args)
    elif args.cmd == "get":
        print(json.dumps(call("GET", f"/api/v1/check/{args.task_id}"), ensure_ascii=False, indent=2))
    elif args.cmd == "status":
        path = f"/api/v1/status/{args.monitor_id}" if args.monitor_id else "/api/v1/status"
        print(json.dumps(call("GET", path), ensure_ascii=False, indent=2))
    elif args.cmd == "summary":
        for f in args.files:
            res = json.loads(Path(f).read_text())
            if args.recompute and res.get("_startedAt"):
                route = res.get("_route")
                if route is None and res.get("_url"):
                    route = "/" + res["_url"].split("/", 3)[3] if res["_url"].count("/") >= 3 else ""
                res["_kiro"] = kiro_stats(datetime.fromisoformat(res["_startedAt"]), datetime.fromisoformat(res["_finishedAt"]), route)
                Path(f).write_text(json.dumps(res, ensure_ascii=False, indent=2))
            print(json.dumps(summarize(res), ensure_ascii=False))


if __name__ == "__main__":
    main()
