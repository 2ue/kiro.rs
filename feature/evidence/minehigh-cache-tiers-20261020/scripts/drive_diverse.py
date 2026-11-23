#!/usr/bin/env python3
"""多样化多轮对话压测：每条会话话题、追问链、上下文规模、模型、流式与否、工具调用都不同。

用法: drive_diverse.py <base_url> <api_key> <out.jsonl> <route>=<conversations> [...]
环境变量: SEED（随机种子）、CONCURRENCY（并发对话数）、REPO（截取代码片段的仓库根目录）
"""
import json
import os
import random
import sys
import threading
import time
import urllib.error
import urllib.request
import uuid
from concurrent.futures import ThreadPoolExecutor

BASE, KEY, OUT = sys.argv[1], sys.argv[2], sys.argv[3]
PLAN = [(arg.split("=")[0], int(arg.split("=")[1])) for arg in sys.argv[4:]]
REPO = os.environ.get("REPO", os.path.abspath(os.path.join(os.path.dirname(__file__), "../../../..")))
CONCURRENCY = int(os.environ.get("CONCURRENCY", "14"))
SEED = int(os.environ.get("SEED", "20261020"))
lock = threading.Lock()


def source_files():
    files = []
    for root in ("src", "ui/src", "admin-ui/src"):
        for dirpath, _, names in os.walk(os.path.join(REPO, root)):
            if "node_modules" in dirpath:
                continue
            for name in names:
                if name.endswith((".rs", ".ts", ".tsx")) and "test" not in name:
                    path = os.path.join(dirpath, name)
                    if os.path.getsize(path) > 6000:
                        files.append(path)
    return sorted(files)


FILES = source_files()


def code_chunk(rng, min_chars=2000, max_chars=12000):
    path = rng.choice(FILES)
    text = open(path, encoding="utf-8", errors="ignore").read()
    size = rng.randint(min_chars, max_chars)
    start = rng.randint(0, max(0, len(text) - size))
    return os.path.relpath(path, REPO), text[start:start + size]


TOOL_DEFS = [
    {"name": "Read", "description": "Read a file from the local filesystem. Returns the file content with line numbers.",
     "input_schema": {"type": "object", "properties": {"file_path": {"type": "string"}, "offset": {"type": "integer"}, "limit": {"type": "integer"}}, "required": ["file_path"]}},
    {"name": "Grep", "description": "Search file contents with a regular expression and return matching lines.",
     "input_schema": {"type": "object", "properties": {"pattern": {"type": "string"}, "path": {"type": "string"}}, "required": ["pattern"]}},
    {"name": "Bash", "description": "Run a shell command and return stdout and stderr.",
     "input_schema": {"type": "object", "properties": {"command": {"type": "string"}, "description": {"type": "string"}}, "required": ["command"]}},
    {"name": "Edit", "description": "Replace an exact string in a file with a new string.",
     "input_schema": {"type": "object", "properties": {"file_path": {"type": "string"}, "old_string": {"type": "string"}, "new_string": {"type": "string"}}, "required": ["file_path", "old_string", "new_string"]}},
]

GUIDE = [
    "Prefer small, reviewable changes and explain trade-offs briefly.",
    "When reading code, cite file paths and line numbers.",
    "Never invent APIs; if unsure, say what you would check.",
    "Keep answers focused; use code blocks for code.",
    "Consider edge cases, error handling and performance.",
    "Match the existing style of the codebase.",
    "Use plain language and avoid filler.",
    "When asked to refactor, keep behavior identical unless told otherwise.",
]


def system_prompt(rng, kind):
    if kind == "none":
        return None
    if kind == "small":
        return "You are a helpful senior engineer. " + " ".join(rng.sample(GUIDE, 3))
    lines = ["You are an interactive CLI coding agent that helps users with software engineering tasks."]
    repeats = 12 if kind == "medium" else 60
    for i in range(repeats):
        lines.append(f"Rule {i + 1}: " + GUIDE[i % len(GUIDE)] + " " + GUIDE[(i * 3 + 1) % len(GUIDE)])
    if kind == "large":
        _, chunk = code_chunk(random.Random(7), 9000, 9000)
        lines.append("Reference conventions excerpt:\n" + chunk)
    return [{"type": "text", "text": "\n".join(lines), "cache_control": {"type": "ephemeral"}}]


TOPICS = [
    ("rust", "Explain how this Rust module works and what its main responsibilities are."),
    ("rust", "Review this Rust code for concurrency or locking problems."),
    ("rust", "Suggest how to reduce allocations in this Rust code."),
    ("ts", "Explain what this TypeScript/React code renders and how state flows."),
    ("ts", "Find accessibility issues in this React component."),
    ("ts", "How would you split this component into smaller pieces?"),
    ("sql", "Design a PostgreSQL schema for a usage-billing ledger with daily rollups."),
    ("sql", "Write a query that finds the top 10 customers by monthly spend growth."),
    ("py", "Write a Python script that tails a log file and aggregates error rates per minute."),
    ("py", "Implement an LRU cache with TTL in Python and explain the complexity."),
    ("ops", "Design a zero-downtime deployment for a Docker Compose service with Postgres and Redis."),
    ("ops", "How should I set up alerting for a proxy service's error rate and latency?"),
    ("algo", "Explain consistent hashing and when to use it over modulo sharding."),
    ("algo", "Compare token bucket and sliding window rate limiting with examples."),
    ("write", "Draft release notes for a feature that adds per-route cache strategies."),
    ("write", "Write a short design doc outline for a multi-tenant API gateway."),
    ("data", "How would you detect anomalies in hourly request counts?"),
    ("data", "Explain p50/p90/p99 latency and how to compute them from histograms."),
    ("sec", "What are common pitfalls when storing API keys and how to avoid them?"),
    ("sec", "Review this code for input validation and injection risks."),
]

FOLLOWUPS = {
    "generic": [
        "Can you go deeper on the second point?",
        "Summarize that in three bullet points.",
        "What edge cases did you not cover yet?",
        "Give me a concrete example with real numbers.",
        "What would you test first, and how?",
        "Rewrite the main part more concisely.",
        "What are the trade-offs of your approach?",
        "If performance mattered most, what would you change?",
        "Explain it as if to a new teammate.",
        "List possible failure modes and how to detect them.",
        "Now write a longer, detailed version with full code and comments.",
        "Compare that with an alternative approach in a table.",
    ],
    "rust": ["Write unit tests for the most important function.", "Where could this panic, and how to avoid it?", "Convert the error handling to use thiserror."],
    "ts": ["Add proper TypeScript types for the props.", "Write a Vitest test for this component.", "How would you memoize the expensive parts?"],
    "sql": ["Add the indexes you would need and explain why.", "How would this behave with 100 million rows?", "Rewrite it using a CTE."],
    "py": ["Add type hints and docstrings.", "Make it async.", "Add CLI arguments with argparse."],
    "ops": ["Write the actual compose file.", "How do I roll back safely?", "What metrics should the dashboard show?"],
    "algo": ["Write a reference implementation.", "Analyze the worst case.", "How does it behave under skewed load?"],
    "write": ["Make it more formal.", "Translate it to Chinese.", "Shorten it to 100 words."],
    "data": ["Write the SQL for it.", "How would you reduce false positives?", "Show a Python version."],
    "sec": ["Show a fixed version of the code.", "How would an attacker exploit it?", "What logging would help detect abuse?"],
}


def post(path, body, stream):
    data = json.dumps({**body, "stream": stream}).encode()
    req = urllib.request.Request(BASE + path, data=data, method="POST", headers={
        "content-type": "application/json", "x-api-key": KEY, "anthropic-version": "2023-06-01"})
    started = time.time()
    try:
        with urllib.request.urlopen(req, timeout=600) as resp:
            raw = resp.read().decode("utf-8", errors="ignore")
            status = resp.status
    except urllib.error.HTTPError as err:
        return {"status": err.code, "error": err.read().decode("utf-8", errors="ignore")[:400], "secs": time.time() - started}
    except Exception as err:  # noqa: BLE001
        return {"status": 0, "error": str(err)[:300], "secs": time.time() - started}
    result = {"status": status, "secs": time.time() - started}
    if not stream:
        msg = json.loads(raw)
        result.update(usage=msg.get("usage"), content=msg.get("content", []), stop=msg.get("stop_reason"))
        return result
    blocks, start_usage, final_usage, stop = {}, None, None, None
    for chunk in raw.split("\n\n"):
        line = next((l for l in chunk.split("\n") if l.startswith("data:")), None)
        if not line:
            continue
        try:
            ev = json.loads(line[5:])
        except ValueError:
            continue
        kind = ev.get("type")
        if kind == "message_start":
            start_usage = ev["message"].get("usage")
        elif kind == "content_block_start":
            block = dict(ev["content_block"])
            if block.get("type") == "tool_use":
                block["_json"] = ""
            blocks[ev["index"]] = block
        elif kind == "content_block_delta":
            block = blocks.setdefault(ev["index"], {"type": "text", "text": ""})
            delta = ev["delta"]
            if delta.get("type") == "text_delta":
                block["text"] = block.get("text", "") + delta.get("text", "")
            elif delta.get("type") == "input_json_delta":
                block["_json"] = block.get("_json", "") + delta.get("partial_json", "")
        elif kind == "message_delta":
            final_usage = ev.get("usage")
            stop = (ev.get("delta") or {}).get("stop_reason")
        elif kind == "error":
            result["error"] = json.dumps(ev.get("error"))[:300]
    content = []
    for index in sorted(blocks):
        block = blocks[index]
        if block.get("type") == "tool_use":
            try:
                block["input"] = json.loads(block.pop("_json") or "{}")
            except ValueError:
                block["input"] = {}
            content.append({k: block[k] for k in ("type", "id", "name", "input")})
        elif block.get("type") == "text":
            content.append({"type": "text", "text": block.get("text", "")})
    result.update(usage=final_usage, start_usage=start_usage, content=content, stop=stop)
    return result


def tool_result(rng, block):
    name = block.get("name")
    if name == "Read":
        path, chunk = code_chunk(rng, 1500, 9000)
        return f"File: {path}\n" + "\n".join(f"{i + 1:>5}\t{line}" for i, line in enumerate(chunk.split("\n")))
    if name == "Grep":
        path, chunk = code_chunk(rng, 600, 2500)
        return "\n".join(f"{path}:{i + 10}:{line}" for i, line in enumerate(chunk.split("\n")[:40]))
    if name == "Bash":
        return "\n".join(rng.choice(["ok", "warning: unused variable", "test result: ok. 12 passed", "Compiling crate v0.1.0", "done in 2.3s"]) for _ in range(rng.randint(3, 25)))
    return "Edit applied successfully."


def conversation(route, index):
    rng = random.Random(f"{SEED}-{route}-{index}")
    family, opener = TOPICS[(index * 7 + hash(route) % 5) % len(TOPICS)]
    sys_kind = rng.choice(["large", "large", "medium", "medium", "small", "none"])
    use_tools = rng.random() < 0.45
    model = "claude-haiku-4.5" if rng.random() < 0.25 else "claude-sonnet-4.5"
    rounds = rng.randint(8, 16)
    session = str(uuid.UUID(int=rng.getrandbits(128), version=4))
    system = system_prompt(rng, sys_kind)
    path, chunk = code_chunk(rng, 3000, 14000)
    first = opener if family in ("sql", "py", "ops", "algo", "write", "data") and rng.random() < 0.5 else f"{opener}\n\nFile `{path}`:\n```\n{chunk}\n```"
    messages = [{"role": "user", "content": [{"type": "text", "text": first}]}]
    pool = FOLLOWUPS["generic"] + FOLLOWUPS.get(family, [])
    rng.shuffle(pool)
    for rnd in range(rounds):
        body = {"model": model, "max_tokens": rng.choice([1024, 2048, 4096, 8192]), "messages": messages,
                "metadata": {"user_id": f"user_e2e_account__session_{session}"}}
        if system:
            body["system"] = system
        if use_tools:
            body["tools"] = TOOL_DEFS
        stream = rng.random() < 0.8
        res = post(f"{route}/v1/messages", body, stream)
        rec = {"route": route, "conv": index, "round": rnd, "model": model, "stream": stream, "sys": sys_kind,
               "tools": use_tools, "status": res.get("status"), "secs": round(res.get("secs", 0), 2),
               "usage": res.get("usage"), "start_usage": res.get("start_usage"), "stop": res.get("stop"),
               "error": res.get("error")}
        with lock:
            with open(OUT, "a") as fh:
                fh.write(json.dumps(rec) + "\n")
        if res.get("status") != 200:
            time.sleep(rng.uniform(2, 5))
            continue
        content = res.get("content") or [{"type": "text", "text": "(no content)"}]
        messages.append({"role": "assistant", "content": content})
        tool_uses = [b for b in content if b.get("type") == "tool_use"]
        if tool_uses:
            messages.append({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": b["id"], "content": tool_result(rng, b)} for b in tool_uses]})
        else:
            follow = pool[rnd % len(pool)]
            if rng.random() < 0.3:
                p2, c2 = code_chunk(rng, 1500, 8000)
                follow = f"{follow}\n\nAlso consider `{p2}`:\n```\n{c2}\n```"
            messages.append({"role": "user", "content": [{"type": "text", "text": follow}]})
        time.sleep(rng.uniform(1.5, 9))


jobs = [(route, i) for route, n in PLAN for i in range(n)]
random.Random(SEED).shuffle(jobs)
with ThreadPoolExecutor(max_workers=CONCURRENCY) as pool:
    list(pool.map(lambda job: conversation(*job), jobs))
print("done", len(jobs))
