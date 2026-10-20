#!/usr/bin/env python3
"""题库回归：每题重复多次，统计稳定性。用法: bank.py <label> <rounds> <model> [case,...]"""
import json, re, sys, time, collections, urllib.request, urllib.error
BASE = "http://127.0.0.1:19322"
KEY = json.load(open("/tmp/kiro-identity-e2e/config.json"))["apiKey"]
WEATHER = [{"name": "get_weather", "description": "Get the current weather for a location.",
            "input_schema": {"type": "object", "properties": {"location": {"type": "string"}, "unit": {"type": "string", "enum": ["celsius", "fahrenheit"]}}, "required": ["location"]}}]
RECENT = ("请回答下面的近期知识题。\n只输出 4 行，每行严格使用“序号|答案”的格式，例如：1|Alaska\n不要输出标题、解释、分析或额外空行。\n\n"
          "1. Q: What is the name of the OpenAI model released on August 7, 2025? Just tell me the name. If you don't know, just answer I don't know.\n\n"
          "2. Q: Who was the American right-wing activist assassinated during a campus event in Utah on September 10, 2025? Just tell me the name. If you don't know, just answer I don't know.\n\n"
          "3. Q: In which U.S. state did President Donald Trump meet Russian President Vladimir Putin on August 15, 2025? Just tell me the name. If you don't know, just answer I don't know.\n\n"
          "4. Q: Who became Japan's first female Prime Minister on October 21, 2025? Just tell me the name. If you don't know, just answer I don't know.")
CASES = {
    # 工具调用
    "tool_forced_text": dict(body={"tools": WEATHER, "messages": [{"role": "user", "content": "You MUST call the get_weather tool to look up the current weather in Exampleville-44c2db3c. Use unit=celsius. Do NOT reply in plain text - only a tool call counts as a valid answer."}]}),
    "tool_choice_tool": dict(body={"tools": WEATHER, "tool_choice": {"type": "tool", "name": "get_weather"}, "messages": [{"role": "user", "content": "What's the weather in Paris?"}]}),
    "tool_choice_any": dict(body={"tools": WEATHER, "tool_choice": {"type": "any"}, "messages": [{"role": "user", "content": "What's the weather in Tokyo in celsius?"}]}),
    # 知识
    "cutoff_ym": dict(body={"messages": [{"role": "user", "content": "What is your training data cutoff date? Reply with ONLY the year and month in format 'YYYY-MM', nothing else. Do not search the web or use any tools."}]}),
    "cutoff_month": dict(body={"messages": [{"role": "user", "content": "What is your knowledge cutoff date? Reply with just the month and year, e.g. 'March 2024'. No additional explanation."}]}),
    "context_window": dict(body={"messages": [{"role": "user", "content": "What is your maximum context window size in tokens? Reply with just a single integer (no commas, no units, no explanation), e.g. 200000."}]}),
    "recent_events": dict(body={"messages": [{"role": "user", "content": RECENT}]}),
    "provider": dict(body={"messages": [{"role": "user", "content": "Output only your provider and model family in one short line, like `Anthropic Claude` or `OpenAI GPT`. No greetings, no extra words."}]}),
    # 透传 / 格式
    "echo_token": dict(body={"messages": [{"role": "user", "content": "Please repeat the following token verbatim in your reply, with no extra characters: f4cc28bc301ba0c0"}]}),
    "echo_tag": dict(body={"messages": [{"role": "user", "content": "我输入了一个tag 是：<dc85d48fe73185ec>。直接输出你前面看到的tag是什么。看到的文本，不要使用任何工具。"}]}),
    "json_minified": dict(body={"messages": [{"role": "user", "content": "You must reply with exactly one minified JSON object and no markdown. Schema: {\"a\": string, \"b\": number, \"c\": string}. Set a to the reverse of 'testz'. Set b to 29 + 8. Set c to 'ZT-A31C5CC2'."}]}),
    "code_exec": dict(body={"tools": [{"type": "code_execution_20250825", "name": "code_execution"}], "messages": [{"role": "user", "content": "Write and execute a Python script that prints 'HELLO_CHECK'. Only use the code execution tool, nothing else."}]}),
}

def call(model, body, stream):
    payload = {"model": model, "max_tokens": 600, "stream": stream, **body}
    req = urllib.request.Request(BASE + "/v1/messages", data=json.dumps(payload).encode(), headers={
        "x-api-key": KEY, "anthropic-version": "2023-06-01", "content-type": "application/json"})
    t0 = time.time()
    try:
        with urllib.request.urlopen(req, timeout=180) as r:
            status, raw = r.status, r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        status, raw = e.code, e.read().decode("utf-8", "replace")
    if status != 200:
        return {"status": status, "error": raw[:200], "secs": round(time.time() - t0, 1)}
    if stream:
        text, tools, stop = "", [], None
        cur = {}
        for blk in raw.split("\n\n"):
            for line in blk.splitlines():
                if not line.startswith("data:"): continue
                try: d = json.loads(line[5:])
                except Exception: continue
                t = d.get("type")
                if t == "content_block_start" and d["content_block"]["type"] == "tool_use":
                    cur[d["index"]] = {"id": d["content_block"]["id"], "name": d["content_block"]["name"], "json": ""}
                elif t == "content_block_delta":
                    dl = d["delta"]
                    if dl.get("type") == "text_delta": text += dl["text"]
                    elif dl.get("type") == "input_json_delta" and d["index"] in cur: cur[d["index"]]["json"] += dl["partial_json"]
                elif t == "message_delta": stop = d["delta"].get("stop_reason")
        for c in cur.values():
            try: inp = json.loads(c["json"] or "{}")
            except Exception: inp = {"_raw": c["json"]}
            tools.append({"id": c["id"], "name": c["name"], "input": inp})
    else:
        d = json.loads(raw)
        text = "".join(b.get("text", "") for b in d["content"] if b["type"] == "text")
        tools = [{"id": b["id"], "name": b["name"], "input": b["input"]} for b in d["content"] if b["type"] == "tool_use"]
        stop = d.get("stop_reason")
    return {"status": status, "text": text.strip(), "tools": tools, "stop": stop, "secs": round(time.time() - t0, 1)}

def main():
    label, rounds, model = sys.argv[1], int(sys.argv[2]), sys.argv[3]
    only = sys.argv[4].split(",") if len(sys.argv) > 4 else list(CASES)
    results = collections.defaultdict(list)
    for rnd in range(rounds):
        for cid in only:
            r = call(model, CASES[cid]["body"], stream=rnd % 2 == 1)
            r["stream"] = rnd % 2 == 1
            results[cid].append(r)
            time.sleep(0.6)
    json.dump(results, open(f"/tmp/kiro-cctest/bank-{label}.json", "w"), ensure_ascii=False, indent=1)
    for cid, rows in results.items():
        if cid.startswith("tool_"):
            summ = collections.Counter(
                "ERR%s" % r["status"] if r["status"] != 200 else
                f"stop={r['stop']},tools={len(r['tools'])},id={r['tools'][0]['id'][:8] if r['tools'] else '-'},input={json.dumps(r['tools'][0]['input'], ensure_ascii=False) if r['tools'] else '-'}"
                for r in rows)
        else:
            summ = collections.Counter(("ERR%s" % r["status"]) if r["status"] != 200 else re.sub(r"\s+", " ", r["text"])[:110] for r in rows)
        print(f"== {cid} ({len(rows)})")
        for k, v in summ.most_common(): print(f"   {v}x {k}")

if __name__ == "__main__":
    main()
