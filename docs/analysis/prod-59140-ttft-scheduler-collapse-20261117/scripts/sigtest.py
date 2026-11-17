#!/usr/bin/env python3
"""本地真实上游的思考签名回放验证（只打本地隔离实例 127.0.0.1:29191）。

用例：
  A 原样回放（基线）：带签名的思考 + tool_use 原样回传，应一次成功
  B 改写思考文本、保留原签名（模拟身份防护改写）：上游应判签名无效
  C 同一会话下一轮仍带 B 的历史：旧版会再失败一次，新版应直接一次成功
  D 改写思考文本、换成合成签名（新版身份防护的输出）：应一次成功
  E display=omitted：空思考文本 + 原签名回放，确认这条路径本身是否会被判无效
每个用例从本地 usage_records 读取上游尝试轨迹。
"""
import json
import subprocess
import sys
import time
import urllib.request
import uuid

BASE = "http://127.0.0.1:29191"
KEY = "sk-kirobench-local-0001"
MODEL = "claude-opus-5-5"
TOOLS = [{
    "name": "get_weather",
    "description": "Get the current weather for a city.",
    "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]},
}]
FIRST = "Think step by step about which tool to use, then call get_weather for Paris. Do not answer without calling the tool."


def post(body, session):
    body = dict(body)
    body["metadata"] = {"user_id": f"user_sigtest_account__session_{session}"}
    req = urllib.request.Request(
        BASE + "/v1/messages", data=json.dumps(body).encode(), method="POST",
        headers={"x-api-key": KEY, "content-type": "application/json", "anthropic-version": "2023-06-01",
                 "user-agent": "claude-cli/2.1.0 (external, cli)"})
    try:
        with urllib.request.urlopen(req, timeout=300) as resp:
            return resp.status, json.loads(resp.read())
    except urllib.error.HTTPError as err:
        return err.code, json.loads(err.read() or b"{}")


def attempts_of(request_id):
    sql = (f"select coalesce(data->'credentialAttempts','[]'::jsonb) from usage_records "
           f"where id='{request_id}'")
    for _ in range(20):
        out = subprocess.run(["docker", "exec", "kirobench-pg", "psql", "-U", "kirobench", "-d", "kirobench_real",
                              "-t", "-A", "-c", sql], capture_output=True, text=True).stdout.strip()
        if out:
            return [a.get("action") for a in json.loads(out)]
        time.sleep(1)
    return ["<no usage record>"]


def first_turn(session, display):
    body = {"model": MODEL, "max_tokens": 4000, "stream": False, "tools": TOOLS,
            "thinking": {"type": "adaptive", "display": display}, "output_config": {"effort": "low"},
            "messages": [{"role": "user", "content": FIRST}]}
    status, resp = post(body, session)
    return status, resp


def replay(session, assistant_content, display, extra_turn=False):
    tool_use = next(b for b in assistant_content if b["type"] == "tool_use")
    messages = [
        {"role": "user", "content": FIRST},
        {"role": "assistant", "content": assistant_content},
        {"role": "user", "content": [{"type": "tool_result", "tool_use_id": tool_use["id"], "content": "Sunny, 21C."}]},
    ]
    if extra_turn:
        messages += [{"role": "assistant", "content": [{"type": "text", "text": "It is sunny in Paris."}]},
                     {"role": "user", "content": "Thanks. Answer in one short sentence: is it warm?"}]
    body = {"model": MODEL, "max_tokens": 2000, "stream": False, "tools": TOOLS,
            "thinking": {"type": "adaptive", "display": display}, "output_config": {"effort": "low"},
            "messages": messages}
    return post(body, session)


def report(label, status, resp):
    rid = resp.get("id") or resp.get("request_id") or (resp.get("error") or {}).get("request_id") or ""
    actions = attempts_of(rid) if rid.startswith("req_") or rid.startswith("msg_") else ["<no id>"]
    print(f"{label}: HTTP {status} id={rid} 上游尝试={actions}")
    return actions


def main():
    sys.path.insert(0, "/root/code/kiro.rs/tmp/kirobench")
    synthetic = __import__("synthetic_sig").synthetic_thinking_signature
    for display in ["summarized", "omitted"]:
        session = str(uuid.uuid4())
        status, resp = first_turn(session, display)
        content = resp.get("content", [])
        kinds = [b["type"] for b in content]
        thinking = next((b for b in content if b["type"] == "thinking"), None)
        print(f"[display={display}] 首轮 HTTP {status} 块={kinds} 思考长度={len((thinking or {}).get('thinking',''))} "
              f"签名长度={len((thinking or {}).get('signature',''))}")
        if status != 200 or thinking is None or "tool_use" not in kinds:
            print("  首轮没有得到 思考+工具调用，跳过该组")
            continue
        if display == "omitted":
            report("  E 空思考+原签名回放", *replay(session, content, display))
            continue
        report("  A 原样回放", *replay(session, content, display))
        modified = json.loads(json.dumps(content))
        for block in modified:
            if block["type"] == "thinking":
                block["thinking"] = block["thinking"] + " (I am Claude Opus 5.5.)"
        session_b = str(uuid.uuid4())
        report("  B 改写思考+原签名", *replay(session_b, modified, display))
        report("  C 同会话下一轮仍带B的历史", *replay(session_b, modified, display, extra_turn=True))
        rewritten = json.loads(json.dumps(modified))
        for block in rewritten:
            if block["type"] == "thinking":
                block["signature"] = synthetic(block["thinking"])
        report("  D 改写思考+合成签名", *replay(str(uuid.uuid4()), rewritten, display))


if __name__ == "__main__":
    main()
