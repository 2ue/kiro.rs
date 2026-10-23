"""Re-run the two cases that hit the old behaviour or failed on the fix, 3 times each per binary."""

import importlib.util
import json
import subprocess
import sys
import time

sys.argv = ["x", "1", "claude-opus-5-5"]
spec = importlib.util.spec_from_file_location("r", "/tmp/acct-e2e/replay_ab.py")
r = importlib.util.module_from_spec(spec)
spec.loader.exec_module(r)

TARGETS = {"15cfed4d#818", "b252e4a8#1036"}
cases = [c for c in r.load_cases() if f"{c['source']}#{c['index']}" in TARGETS]
out = {}
for name, binary in r.MODES.items():
    started = subprocess.run([f"{r.ROOT}/start.sh", binary], capture_output=True, text=True, timeout=200)
    assert "started" in started.stdout, started.stdout
    time.sleep(3)
    for case in cases:
        key = f"{case['source']}#{case['index']}"
        for attempt in range(3):
            body = {
                "model": r.MODEL,
                "max_tokens": 3000,
                "thinking": {"type": "enabled", "budget_tokens": 1500},
                "system": r.SYSTEM,
                "tools": r.tools_for(case["messages"]),
                "messages": case["messages"],
            }
            status, reply = r.call(body)
            if status != 200:
                time.sleep(8)
                status, reply = r.call(body)
            hits = []
            for block in reply.get("content") or []:
                text = block.get("text") or block.get("thinking") or ""
                hits += [(block.get("type"), m.group(0)) for m in r.LEAK.finditer(text)]
            out.setdefault(name, []).append({"case": key, "status": status, "hits": hits})
            print(f"[{name}] {key} #{attempt} status={status} hits={hits}", flush=True)
            time.sleep(5)
json.dump(out, open(f"{r.ROOT}/rerun-opus.json", "w"), ensure_ascii=False, indent=2)
