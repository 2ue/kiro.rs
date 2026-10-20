#!/usr/bin/env python3
"""协议兼容改造的真实账号回归。用法: e2e.py [group,...]，group: so tools ws usage

前提：本地隔离实例 127.0.0.1:19322，key 从 /tmp/kiro-identity-e2e/config.json 读取。
脚本会把 /cc 路由的 WebSearch 模型驱动开关打开，结束时还原。
"""
import copy, json, re, sys, time, urllib.request, urllib.error

BASE = "http://127.0.0.1:19322"
CFG = json.load(open("/tmp/kiro-identity-e2e/config.json"))
KEY, ADMIN = CFG["apiKey"], CFG["adminApiKey"]
MODEL = "claude-sonnet-4-5"
RESULTS = []

PERSON = {"type": "object", "properties": {"name": {"type": "string"}, "age": {"type": "integer"},
          "tags": {"type": "array", "items": {"type": "string"}}},
          "required": ["name", "age", "tags"], "additionalProperties": False}
NESTED = {"type": "object", "properties": {
    "city": {"type": "string"},
    "forecast": {"type": "array", "items": {"type": "object", "properties": {
        "day": {"type": "string", "enum": ["mon", "tue", "wed"]}, "high": {"type": "integer"}},
        "required": ["day", "high"], "additionalProperties": False}}},
    "required": ["city", "forecast"], "additionalProperties": False}
WEATHER = [{"name": "get_weather", "description": "Get the current weather for a city.",
            "input_schema": {"type": "object", "properties": {"city": {"type": "string"},
                             "unit": {"type": "string", "enum": ["celsius", "fahrenheit"]}}, "required": ["city"]}}]
WS = [{"type": "web_search_20250305", "name": "web_search", "max_uses": 3}]
TOOL_ID = re.compile(r"^toolu_01[1-9A-HJ-NP-Za-km-z]{22}$")
SRV_ID = re.compile(r"^srvtoolu_01[1-9A-HJ-NP-Za-km-z]{22}$")


def check(name, ok, detail=""):
    RESULTS.append((name, bool(ok)))
    print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f"  {detail}" if detail else ""), flush=True)


def http(method, url, body=None, headers=None, timeout=240):
    req = urllib.request.Request(url, method=method, data=json.dumps(body).encode() if body is not None else None,
                                 headers={"content-type": "application/json", **(headers or {})})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read().decode("utf-8", "replace"), dict(r.headers)
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace"), dict(e.headers)


def post(body, prefix="", extra_headers=None):
    body = {"model": MODEL, "max_tokens": 1024, **body}
    headers = {"x-api-key": KEY, "anthropic-version": "2023-06-01", **(extra_headers or {})}
    for attempt in range(4):
        st, raw, hdr = http("POST", BASE + prefix + "/v1/messages", body, headers)
        if st in (429, 503) and attempt < 3:
            time.sleep(12)
            continue
        return st, raw, hdr
    return st, raw, hdr


def parse_sse(raw):
    events = []
    for block in raw.split("\n\n"):
        name, data = None, None
        for line in block.splitlines():
            if line.startswith("event:"):
                name = line[6:].strip()
            elif line.startswith("data:"):
                data = json.loads(line[5:])
        if data is not None:
            events.append((name, data))
    return events


def assemble(events):
    """Rebuilds the message from SSE events like the official SDK does."""
    msg, blocks = None, {}
    for _, e in events:
        t = e["type"]
        if t == "message_start":
            msg = copy.deepcopy(e["message"])
        elif t == "content_block_start":
            b = dict(e["content_block"])
            if b["type"] in ("tool_use", "server_tool_use"):
                b["_json"] = ""
            if b["type"] == "text":
                b["citations"] = list(b.get("citations") or [])
            blocks[e["index"]] = b
        elif t == "content_block_delta":
            b, d = blocks[e["index"]], e["delta"]
            if d["type"] == "text_delta":
                b["text"] += d["text"]
            elif d["type"] == "input_json_delta":
                b["_json"] += d["partial_json"]
            elif d["type"] == "citations_delta":
                b["citations"].append(d["citation"])
            elif d["type"] == "thinking_delta":
                b["thinking"] = b.get("thinking", "") + d["thinking"]
        elif t == "message_delta":
            msg["stop_reason"] = e["delta"].get("stop_reason")
            msg["usage"] = {**msg.get("usage", {}), **e.get("usage", {})}
    for b in blocks.values():
        if "_json" in b:
            j = b.pop("_json")
            b["input"] = json.loads(j) if j else b.get("input", {})
        if b.get("type") == "text" and not b.get("citations"):
            b.pop("citations", None)
    msg["content"] = [blocks[i] for i in sorted(blocks)]
    return msg


def call(body, prefix="", stream=False, extra_headers=None):
    st, raw, hdr = post({**body, "stream": stream}, prefix, extra_headers)
    if st != 200:
        return st, None, raw
    if stream:
        events = parse_sse(raw)
        return st, assemble(events), events
    return st, json.loads(raw), None


def stream_shape_ok(events):
    names = [n for n, _ in events if n != "ping"]
    if not names or names[0] != "message_start" or names[-1] != "message_stop":
        return False, f"first/last {names[:1]} {names[-1:]}"
    if names.count("message_start") != 1 or names.count("message_stop") != 1:
        return False, "duplicate start/stop"
    open_blocks, seen = set(), set()
    for n, e in events:
        if n == "content_block_start":
            if e["index"] in seen:
                return False, f"index {e['index']} reused"
            seen.add(e["index"]); open_blocks.add(e["index"])
        elif n == "content_block_delta" and e["index"] not in open_blocks:
            return False, f"delta on closed block {e['index']}"
        elif n == "content_block_stop":
            open_blocks.discard(e["index"])
        if n and e.get("type") != n:
            return False, f"event name {n} != type {e.get('type')}"
    if seen != set(range(len(seen))):
        return False, f"indexes not contiguous {sorted(seen)}"
    return not open_blocks, f"open {open_blocks}" if open_blocks else ""


def text_of(msg):
    return "".join(b.get("text", "") for b in msg["content"] if b["type"] == "text")


def set_websearch(overrides):
    hdr = {"x-api-key": ADMIN}
    _, raw, _ = http("GET", BASE + "/api/admin/config/runtime", headers=hdr)
    cfg = json.loads(raw); cfg.pop("promptSteeringDefaults", None)
    cfg["bodyConversion"]["modelDrivenWebSearch"] = {"enabled": False, "pathOverrides": overrides}
    st, _, _ = http("PUT", BASE + "/api/admin/config/runtime", cfg, hdr)
    return st


# ---------------------------------------------------------------- structured output
def group_so():
    for stream in (False, True):
        tag = "流式" if stream else "非流式"
        st, msg, extra = call({"messages": [{"role": "user", "content": "Generate a fictional person."}],
                               "output_config": {"format": {"type": "json_schema", "schema": PERSON}}}, stream=stream)
        if st != 200:
            check(f"SO {tag} HTTP 200", False, extra[:200]); continue
        try:
            obj = json.loads(text_of(msg)); parsed = True
        except Exception:
            obj, parsed = {}, False
        check(f"SO {tag}: 只有一个 text 块", [b["type"] for b in msg["content"]] == ["text"], [b["type"] for b in msg["content"]])
        check(f"SO {tag}: 纯 JSON 可解析且字段完全符合 schema", parsed and set(obj) == {"name", "age", "tags"}
              and isinstance(obj.get("age"), int) and isinstance(obj.get("tags"), list), text_of(msg)[:120])
        check(f"SO {tag}: stop_reason=end_turn", msg["stop_reason"] == "end_turn", msg["stop_reason"])
        if stream:
            ok, why = stream_shape_ok(extra)
            check("SO 流式: 事件结构合法", ok, why)

    st, msg, _ = call({"messages": [{"role": "user", "content": "Give a 3-day forecast for a fictional city."}],
                       "output_config": {"format": {"type": "json_schema", "schema": NESTED}}})
    obj = json.loads(text_of(msg)) if st == 200 else {}
    days = [d.get("day") for d in obj.get("forecast", [])]
    check("SO 嵌套 schema + enum", st == 200 and isinstance(obj.get("city"), str) and days
          and all(d in ("mon", "tue", "wed") for d in days), text_of(msg)[:160] if msg else st)

    st, msg, extra = call({"messages": [{"role": "user", "content": "Generate a fictional person."}],
                           "output_format": {"type": "json_schema", "schema": PERSON}},
                          extra_headers={"anthropic-beta": "structured-outputs-2025-11-13"})
    ok = st == 200 and msg["stop_reason"] == "end_turn"
    try:
        ok = ok and set(json.loads(text_of(msg))) == {"name", "age", "tags"}
    except Exception:
        ok = False
    check("SO 旧版 output_format", ok, (text_of(msg)[:100] if msg else extra[:160]))

    st, msg, extra = call({"messages": [{"role": "user", "content": "Generate a fictional person."}],
                           "thinking": {"type": "enabled", "budget_tokens": 2048}, "max_tokens": 4096,
                           "output_config": {"format": {"type": "json_schema", "schema": PERSON}}})
    texts = [b for b in msg["content"] if b["type"] == "text"] if msg else []
    ok = st == 200 and len(texts) == 1 and msg["stop_reason"] == "end_turn"
    try:
        ok = ok and set(json.loads(texts[0]["text"])) == {"name", "age", "tags"}
    except Exception:
        ok = False
    check("SO + thinking: thinking 之后一个 JSON text 块", ok,
          [b["type"] for b in msg["content"]] if msg else extra[:160])

    st, msg, _ = call({"tools": WEATHER, "messages": [{"role": "user", "content": "What's the weather in Paris right now? Use the tool."}],
                       "output_config": {"format": {"type": "json_schema", "schema": PERSON}}})
    check("SO + 用户工具（需要工具）: 返回 tool_use，stop=tool_use", st == 200 and msg["stop_reason"] == "tool_use"
          and any(b["type"] == "tool_use" and b["name"] == "get_weather" for b in msg["content"]),
          [b.get("name", b["type"]) for b in msg["content"]] if msg else st)
    st, msg, _ = call({"tools": WEATHER, "messages": [{"role": "user", "content": "Generate a fictional person. Do not call any tool."}],
                       "output_config": {"format": {"type": "json_schema", "schema": PERSON}}})
    ok = st == 200 and msg["stop_reason"] == "end_turn"
    try:
        ok = ok and set(json.loads(text_of(msg))) == {"name", "age", "tags"}
    except Exception:
        ok = False
    check("SO + 用户工具（不需要工具）: 直接 JSON", ok, text_of(msg)[:100] if msg else st)


# ---------------------------------------------------------------- tool ids
def group_tools():
    for choice in (None, {"type": "any"}, {"type": "tool", "name": "get_weather"}):
        for stream in (False, True):
            body = {"tools": WEATHER, "messages": [{"role": "user", "content": "What's the weather in Paris in celsius?"}]}
            if choice:
                body["tool_choice"] = choice
            st, msg, extra = call(body, stream=stream)
            label = f"工具 tool_choice={choice and choice['type']} {'流式' if stream else '非流式'}"
            uses = [b for b in msg["content"] if b["type"] == "tool_use"] if msg else []
            check(f"{label}: id=toolu_01+22 base58, stop=tool_use", st == 200 and uses
                  and all(TOOL_ID.match(b["id"]) for b in uses) and msg["stop_reason"] == "tool_use"
                  and uses[0]["name"] == "get_weather" and isinstance(uses[0]["input"], dict),
                  [b.get("id") for b in uses] if msg else extra[:160])
            if stream and msg:
                ok, why = stream_shape_ok(extra)
                check(f"{label}: 事件结构合法", ok, why)
            if not stream and choice is None and uses:
                st2, msg2, _ = call({"tools": WEATHER, "messages": [
                    body["messages"][0],
                    {"role": "assistant", "content": msg["content"]},
                    {"role": "user", "content": [{"type": "tool_result", "tool_use_id": uses[0]["id"],
                                                  "content": "18°C, sunny"}]}]})
                check("工具第二轮 tool_result 回传", st2 == 200 and msg2["stop_reason"] == "end_turn"
                      and "18" in text_of(msg2), text_of(msg2)[:100] if msg2 else st2)


# ---------------------------------------------------------------- websearch
def group_ws():
    st = set_websearch({"/cc": True})
    check("开启 /cc 模型驱动 WebSearch", st == 200, st)
    question = "Search the web: what is the latest stable version of the Rust programming language?"

    st, msg, _ = call({"tools": WS, "messages": [{"role": "user", "content": question}]})
    srv = [b for b in msg["content"] if b["type"] == "server_tool_use"] if msg else []
    check("模板模式 /v1: srvtoolu_01 id + web_search_requests=1", st == 200 and srv and SRV_ID.match(srv[0]["id"])
          and msg["usage"].get("server_tool_use", {}).get("web_search_requests") == 1,
          (srv[0]["id"] if srv else None, msg["usage"] if msg else st))

    for stream in (False, True):
        tag = "流式" if stream else "非流式"
        st, msg, extra = call({"tools": WS, "messages": [{"role": "user", "content": question}]}, "/cc", stream)
        if st != 200:
            check(f"模型驱动 {tag} HTTP 200", False, extra[:200]); continue
        kinds = [b["type"] for b in msg["content"]]
        srv = [b for b in msg["content"] if b["type"] == "server_tool_use"]
        results = [b for b in msg["content"] if b["type"] == "web_search_tool_result"]
        cites = [c for b in msg["content"] if b["type"] == "text" for c in b.get("citations", [])]
        query = srv[0]["input"].get("query", "") if srv else ""
        check(f"模型驱动 {tag}: server_tool_use→result→text 顺序", srv and results
              and kinds.index("server_tool_use") < kinds.index("web_search_tool_result") < len(kinds) - 1
              and kinds[-1] == "text", kinds)
        check(f"模型驱动 {tag}: 搜索词由模型生成（不是原句）", query and query != question
              and not query.lower().startswith("search the web"), query)
        check(f"模型驱动 {tag}: id 配对且为 srvtoolu_01", all(SRV_ID.match(b["id"]) for b in srv)
              and [r["tool_use_id"] for r in results] == [b["id"] for b in srv])
        check(f"模型驱动 {tag}: 结果非空", all(isinstance(r["content"], list) and r["content"] for r in results))
        check(f"模型驱动 {tag}: 带 web_search_result_location 引用", cites and all(
            c["type"] == "web_search_result_location" and c["url"].startswith("http") and c.get("encrypted_index")
            and len(c.get("cited_text", "")) <= 150 for c in cites), len(cites))
        check(f"模型驱动 {tag}: 正文没有残留 [n] 标记", not re.search(r"\[\d{1,3}\]", text_of(msg)))
        check(f"模型驱动 {tag}: 不是模板回答", "Here are the search results" not in text_of(msg)
              and "I'll search for" not in text_of(msg), text_of(msg)[:80])
        u = msg["usage"]
        check(f"模型驱动 {tag}: usage 累加（input>1000）且有搜索次数", u.get("input_tokens", 0) > 1000
              and u.get("output_tokens", 0) > 0 and u.get("server_tool_use", {}).get("web_search_requests") == len(srv), u)
        check(f"模型驱动 {tag}: stop_reason=end_turn", msg["stop_reason"] == "end_turn", msg["stop_reason"])
        if stream:
            ok, why = stream_shape_ok(extra)
            check("模型驱动 流式: 事件结构合法", ok, why)
            start_usage = extra[0][1]["message"]["usage"]
            check("模型驱动 流式: message_start 不带 server_tool_use", "server_tool_use" not in start_usage, start_usage)
        else:
            history = msg["content"]

    st, msg, _ = call({"tools": WS, "messages": [{"role": "user", "content": "Perform a web search for the query: AI news 2026-07-04"}]}, "/cc")
    srv = [b for b in msg["content"] if b["type"] == "server_tool_use"] if msg else []
    check("cctest #3 原题: 有搜索、有引用", st == 200 and srv and any(b.get("citations") for b in msg["content"]),
          srv[0]["input"] if srv else st)

    st, msg, _ = call({"tools": [{**WS[0], "max_uses": 1}], "messages": [{"role": "user", "content":
                       "Search separately for the population of Tokyo and the population of Paris, then compare them."}]}, "/cc")
    n = sum(1 for b in msg["content"] if b["type"] == "server_tool_use") if msg else 0
    ok_results = sum(1 for b in msg["content"] if b["type"] == "web_search_tool_result" and isinstance(b["content"], list)) if msg else 0
    check("max_uses=1: 只执行 1 次搜索，超出部分返回 max_uses_exceeded", st == 200 and ok_results == 1
          and msg["usage"]["server_tool_use"]["web_search_requests"] == 1 and msg["stop_reason"] == "end_turn",
          f"server_tool_use={n} executed={ok_results}")

    st, msg, _ = call({"tools": WS, "messages": [{"role": "user", "content": "What is 2+2? Answer directly."}]}, "/cc")
    check("不需要搜索的问题: 模型可以不搜", st == 200 and msg["stop_reason"] == "end_turn" and "4" in text_of(msg),
          [b["type"] for b in msg["content"]] if msg else st)

    if "history" in locals() or True:
        st, msg0, _ = call({"tools": WS, "messages": [{"role": "user", "content": question}]}, "/cc")
        if st == 200:
            st2, msg2, raw2 = call({"tools": WS, "messages": [
                {"role": "user", "content": question},
                {"role": "assistant", "content": msg0["content"]},
                {"role": "user", "content": "Thanks. In one sentence, what did you find?"}]}, "/cc")
            check("多轮: 历史带 server_tool_use/result/citations 回传", st2 == 200 and text_of(msg2).strip(),
                  text_of(msg2)[:100] if msg2 else raw2[:200])

    st, msg, _ = call({"tools": WS + WEATHER, "messages": [{"role": "user", "content":
                       "Use get_weather to get the weather in Paris. Do not search the web."}]}, "/cc")
    check("混用 web_search + 用户工具: 用户工具调用正常返回", st == 200 and msg["stop_reason"] == "tool_use"
          and any(b["type"] == "tool_use" and b["name"] == "get_weather" and TOOL_ID.match(b["id"]) for b in msg["content"]),
          [b.get("name", b["type"]) for b in msg["content"]] if msg else st)

    st = set_websearch({})
    check("还原 WebSearch 开关", st == 200, st)


# ---------------------------------------------------------------- usage records
def group_usage():
    set_websearch({"/cc": True})
    st, msg, _ = call({"tools": WS, "messages": [{"role": "user", "content": "Search the web: Rust 1.90 release date"}]}, "/cc")
    time.sleep(3)
    _, raw, _ = http("GET", BASE + "/api/admin/usage-records?limit=5", headers={"x-api-key": ADMIN})
    records = json.loads(raw)["records"]
    rec = next((r for r in records if r.get("endpoint", "").startswith("/cc")), None)
    check("usage 记录: 模型驱动请求成功入库", rec and rec["status"] == "success", rec and rec["status"])
    if rec and msg:
        check("usage 记录: input/output 与响应一致", rec["outputTokens"] == msg["usage"]["output_tokens"]
              and rec["totalInputTokens"] >= msg["usage"]["input_tokens"],
              {k: rec[k] for k in ("totalInputTokens", "outputTokens", "cacheReadInputTokens")} | {"resp": msg["usage"]})
        raw_usage = rec.get("rawUsage") or {}
        check("usage 记录: rawUsage 为累加值（>1000）", (raw_usage.get("inputTokens") or raw_usage.get("input_tokens") or 0) > 1000, raw_usage)
    set_websearch({})


GROUPS = {"so": group_so, "tools": group_tools, "ws": group_ws, "usage": group_usage}
if __name__ == "__main__":
    for name in (sys.argv[1].split(",") if len(sys.argv) > 1 else list(GROUPS)):
        print(f"\n===== {name}", flush=True)
        try:
            GROUPS[name]()
        except Exception as error:  # keep later groups running
            check(f"{name} 组异常", False, repr(error))
    if any(n.startswith("开启") for n, _ in RESULTS):
        set_websearch({})
    failed = [n for n, ok in RESULTS if not ok]
    print(f"\n合计 {len(RESULTS)} 项，通过 {len(RESULTS) - len(failed)}，失败 {len(failed)}")
    for n in failed:
        print("  FAIL:", n)
    sys.exit(1 if failed else 0)
