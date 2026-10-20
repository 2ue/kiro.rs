import json, sys, time, urllib.request, urllib.error
BASE = "http://127.0.0.1:19322"
KEY = json.load(open("/tmp/kiro-identity-e2e/config.json"))["apiKey"]
MODEL = sys.argv[1] if len(sys.argv) > 1 else "claude-sonnet-4-5"

def post(body, stream=False):
    body = {"model": MODEL, **body}
    if stream: body["stream"] = True
    req = urllib.request.Request(BASE + "/v1/messages", data=json.dumps(body).encode(), headers={
        "x-api-key": KEY, "anthropic-version": "2023-06-01", "content-type": "application/json",
        "anthropic-beta": "structured-outputs-2025-11-13"})
    try:
        with urllib.request.urlopen(req, timeout=180) as r:
            return r.status, r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")

def events(raw):
    out = []
    for block in raw.split("\n\n"):
        ev = data = None
        for line in block.splitlines():
            if line.startswith("event:"): ev = line[6:].strip()
            elif line.startswith("data:"):
                try: data = json.loads(line[5:])
                except Exception: data = line[5:]
        if ev or data: out.append((ev, data))
    return out

case = sys.argv[2]
if case == "structured":
    schema = {"type": "object", "properties": {"name": {"type": "string"}, "age": {"type": "integer"}, "tags": {"type": "array", "items": {"type": "string"}}},
              "required": ["name", "age", "tags"], "additionalProperties": False}
    st, raw = post({"max_tokens": 400, "messages": [{"role": "user", "content": "Generate a fictional person."}],
                    "output_config": {"format": {"type": "json_schema", "schema": schema}}})
    print("HTTP", st)
    d = json.loads(raw)
    if st != 200: print(raw[:400]); sys.exit()
    text = "".join(b.get("text", "") for b in d["content"] if b["type"] == "text")
    print("blocks", [b["type"] for b in d["content"]], "stop", d.get("stop_reason"))
    print("text:", text[:300])
    try:
        obj = json.loads(text); print("纯 JSON 可解析:", True, "keys", sorted(obj), "额外字段:", sorted(set(obj) - {"name", "age", "tags"}))
        print("age 是整数:", isinstance(obj.get("age"), int))
    except Exception as e:
        print("纯 JSON 可解析: False", e)
elif case == "tools":
    tools = [{"name": "get_weather", "description": "Get the current weather for a city.",
              "input_schema": {"type": "object", "properties": {"city": {"type": "string"}, "unit": {"type": "string", "enum": ["celsius", "fahrenheit"]}}, "required": ["city"]}}]
    for choice in [None, {"type": "any"}, {"type": "tool", "name": "get_weather"}]:
        body = {"max_tokens": 400, "tools": tools, "messages": [{"role": "user", "content": "What's the weather in Paris in celsius?"}]}
        if choice: body["tool_choice"] = choice
        st, raw = post(body)
        d = json.loads(raw)
        if st != 200: print(choice, "HTTP", st, raw[:300]); continue
        tu = [b for b in d["content"] if b["type"] == "tool_use"]
        print("tool_choice=", choice and choice["type"], "| stop", d.get("stop_reason"), "| blocks", [b["type"] for b in d["content"]],
              "| id", tu and tu[0]["id"][:12], "| input", tu and tu[0]["input"])
        if tu and choice is None:
            # second turn with tool_result
            st2, raw2 = post({"max_tokens": 300, "tools": tools, "messages": [
                {"role": "user", "content": "What's the weather in Paris in celsius?"},
                {"role": "assistant", "content": d["content"]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tu[0]["id"], "content": "18 C, sunny"}]}]})
            d2 = json.loads(raw2)
            print("  tool_result 回合: HTTP", st2, "stop", d2.get("stop_reason"), "text", "".join(b.get("text", "") for b in d2.get("content", []))[:120])
    # streaming tool use
    st, raw = post({"max_tokens": 400, "tools": tools, "tool_choice": {"type": "any"}, "messages": [{"role": "user", "content": "Weather in Tokyo?"}]}, stream=True)
    evs = events(raw)
    starts = [d.get("content_block", {}).get("type") for e, d in evs if e == "content_block_start"]
    deltas = sorted({d.get("delta", {}).get("type") for e, d in evs if e == "content_block_delta"})
    md = [d for e, d in evs if e == "message_delta"]
    print("流式工具: blocks", starts, "deltas", deltas, "stop", md and md[-1].get("delta", {}).get("stop_reason"))
elif case == "websearch":
    for stream in (False, True):
        st, raw = post({"max_tokens": 1024, "messages": [{"role": "user", "content": "Search the web: what is the latest stable version of the Rust programming language?"}],
                        "tools": [{"type": "web_search_20250305", "name": "web_search", "max_uses": 2}]}, stream=stream)
        print("stream" if stream else "non-stream", "HTTP", st)
        if st != 200: print(raw[:400]); continue
        if stream:
            evs = events(raw)
            blocks = [d.get("content_block", {}) for e, d in evs if e == "content_block_start"]
            print("  blocks", [b.get("type") for b in blocks])
            srv = [b for b in blocks if b.get("type") == "server_tool_use"]
            print("  server_tool_use", srv and {k: srv[0].get(k) for k in ("id", "name", "input")})
            md = [d for e, d in evs if e == "message_delta"]
            print("  usage", md and md[-1].get("usage"))
        else:
            d = json.loads(raw)
            print("  blocks", [b["type"] for b in d["content"]])
            for b in d["content"]:
                if b["type"] == "server_tool_use": print("  server_tool_use", {k: b.get(k) for k in ("id", "name", "input")})
                if b["type"] == "web_search_tool_result":
                    c = b.get("content")
                    print("  result", type(c).__name__, (c[0] if isinstance(c, list) and c else c) if not isinstance(c, list) else {k: c[0].get(k) for k in ("type", "url", "title")} if c else "空")
                if b["type"] == "text" and b.get("citations"): print("  citations 示例", b["citations"][0].get("type"))
            print("  usage.server_tool_use", d.get("usage", {}).get("server_tool_use"))
elif case == "stream":
    for i in range(int(sys.argv[3]) if len(sys.argv) > 3 else 3):
        st, raw = post({"max_tokens": 200, "messages": [{"role": "user", "content": "Count from 1 to 5."}]}, stream=True)
        evs = events(raw)
        names = [e for e, _ in evs]
        seq = [n for n in names if n != "ping"]
        ms = next((d for e, d in evs if e == "message_start"), {})
        issues = []
        if not seq or seq[0] != "message_start": issues.append("首事件不是 message_start")
        if seq and seq[-1] != "message_stop": issues.append("末事件不是 message_stop")
        if "ping" not in names: issues.append("无 ping")
        msg = ms.get("message", {}) if isinstance(ms, dict) else {}
        for k in ("id", "type", "role", "model", "content", "stop_reason", "stop_sequence", "usage"):
            if k not in msg: issues.append("message_start 缺 " + k)
        u = msg.get("usage", {})
        md = [d for e, d in evs if e == "message_delta"]
        mdu = md[-1].get("usage", {}) if md else {}
        print(f"#{i} HTTP {st} 事件 {' > '.join(dict.fromkeys(seq))} | ping={'ping' in names} | start.usage={u} | delta.usage={mdu} | 问题={issues or '无'}")
elif case == "thinking":
    n = int(sys.argv[3]) if len(sys.argv) > 3 else 3
    for i in range(n):
        stream = i % 2 == 1
        st, raw = post({"max_tokens": 2048, "thinking": {"type": "enabled", "budget_tokens": 1024},
                        "messages": [{"role": "user", "content": "What is 17 * 23? Think first."}]}, stream=stream)
        if st != 200: print(i, "HTTP", st, raw[:200]); continue
        if stream:
            evs = events(raw)
            sigs = [d["delta"]["signature"] for e, d in evs if e == "content_block_delta" and d.get("delta", {}).get("type") == "signature_delta"]
            blocks = [d.get("content_block", {}).get("type") for e, d in evs if e == "content_block_start"]
            sig = sigs[-1] if sigs else ""
        else:
            d = json.loads(raw)
            blocks = [b["type"] for b in d["content"]]
            sig = next((b.get("signature", "") for b in d["content"] if b["type"] == "thinking"), "")
        print(f"#{i} {'S' if stream else 'N'} blocks={blocks} sig_len={len(sig)} sig_head={sig[:16]!r}")
elif case == "knowledge":
    st, raw = post({"max_tokens": 200, "messages": [{"role": "user", "content": "What is your knowledge cutoff date? Answer with month and year only."}]})
    print("HTTP", st, "".join(b.get("text", "") for b in json.loads(raw).get("content", [])))
