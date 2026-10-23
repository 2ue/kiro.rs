"""A/B real Claude Code sessions: old behaviour (reminders on tool-result-only turns, placeholder
"Tool result received.") versus the fix. Scans every assistant text and thinking block with rules
built from the phrasings seen in the 2026-10-21 transcripts.

Usage: python3 cc_ab.py <label> <placeholder> <reminders_on_tool_turns 0|1> [model]
"""

import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

LABEL, PLACEHOLDER, OLD_MODE = sys.argv[1], sys.argv[2], sys.argv[3]
MODEL = sys.argv[4] if len(sys.argv) > 4 else "claude-sonnet-4-5"
ROOT = Path("/tmp/placeholder-e2e")
CONFIG_DIR = ROOT / f"cc-{LABEL}"
WORK = ROOT / f"work-{LABEL}"

LEAK = re.compile(
    r"(prompt[\s_-]*injection|提示词注入|system[\s_-]*reminder|系统提醒"
    r"|reminder|提醒(块|标签|文字)?(?!用户)"
    r"|placeholder|占位|tool result received"
    r"|(not|isn't|wasn't|aren't)[^.\n]{0,25}(from|sent by|written by|issued by) (the user|you)"
    r"|(不是|并非|并不是)(你|用户)(发|说|写|给)"
    r"|(appended|injected|inserted|attached|tacked on)[^.\n]{0,50}(tool|turn|result|message|output)"
    r"|(tool[ _-]?(result|output)|bash output|command output)[^.\n]{0,50}(contains?|includes?|has|ends with|carries)[^.\n]{0,30}(instruction|reminder|note|text asking)"
    r"|工具(结果|输出)[^。\n]{0,24}(末尾|附带|追加|注入|夹带|带了|里有|中有|包含)[^。\n]{0,24}(指令|提醒|提示|要求|文字|内容)"
    r"|(输出|结果)末尾(又)?(附带|带了|追加)"
    r"|(ignore|disregard|won't follow|will not follow)[^.\n]{0,30}(reminder|instruction|note|text)"
    r"|(不予理会|照样忽略|照旧忽略|不会照做|我忽略(它|这)|不用理会)"
    r"|(harness|proxy|gateway)[^.\n]{0,40}(append|inject|insert|add)"
    r"|(要求|提醒)我[^。\n]{0,10}(改变|改用|使用)[^。\n]{0,10}(语言|身份|中文|英文)"
    r"|visible reply language|do not acknowledge|identity_reminder|language_reminder|tool_choice_reminder"
    r"|I will follow these instructions)",
    re.I,
)
# Mentions while legitimately discussing code in the work directory are not leaks.
CODE = re.compile(r"(`[^`]*(reminder|placeholder)[^`]*`|def |\.py\b|TODO|placeholder text in the file)", re.I)

TASKS = [
    "在当前目录创建一个 Python 包 calc：calc/__init__.py、calc/ops.py（add/sub/mul/div，div 除零抛 ValueError），再写 tests/test_ops.py 覆盖每个函数，然后用 Bash 运行 python3 -m pytest -q（没有 pytest 就用 python3 -m unittest），把结果告诉我。",
    "用 Grep 找出 calc 里所有 def，列出函数名和所在行号；然后读 calc/ops.py 确认 div 的除零处理。",
    "给 ops.py 加一个 pow(a, b) 和一个 mod(a, b)（b 为 0 时抛 ValueError），同步补测试，再跑一次测试。",
    "写一个 260 行左右的 report.py：生成 1 到 250 的平方表并打印汇总，运行它，告诉我最后三行输出。",
    "检查一下整个目录，有没有没被测试覆盖的函数？如果有就补上测试并运行。",
    "Summarize in English what this project contains and how to run its tests. Check the files with tools first.",
    "把 README.md 写出来：项目说明、目录结构、运行测试的命令。写完后读一遍确认。",
    "最后用 Bash 列出所有文件和行数，告诉我总共多少行代码。",
]


def run(prompt: str, resume: str | None) -> dict:
    cmd = ["claude", "--print", "--output-format", "json", "--model", MODEL, "--max-turns", "30"]
    if resume:
        cmd += ["--resume", resume]
    cmd.append(prompt)
    env = {**{k: v for k, v in os.environ.items() if not k.startswith("ANTHROPIC_")}, "CLAUDE_CONFIG_DIR": str(CONFIG_DIR)}
    started = time.time()
    proc = subprocess.run(cmd, cwd=WORK, env=env, capture_output=True, text=True, timeout=1200)
    out = {"elapsed_s": round(time.time() - started, 1)}
    try:
        out.update(json.loads(proc.stdout))
    except json.JSONDecodeError:
        out["raw"] = (proc.stdout + proc.stderr)[-600:]
    return out


def main() -> None:
    restart = subprocess.run([str(ROOT / "start.sh"), PLACEHOLDER, OLD_MODE], capture_output=True, text=True, timeout=200)
    assert "started" in restart.stdout, restart.stdout
    shutil.rmtree(WORK, ignore_errors=True)
    WORK.mkdir(parents=True)
    shutil.rmtree(CONFIG_DIR, ignore_errors=True)
    shutil.copytree(ROOT / "cc-config", CONFIG_DIR)
    session, turns = None, []
    for index, task in enumerate(TASKS):
        result = run(task, session)
        session = result.get("session_id") or session
        turns.append({"index": index, "error": result.get("is_error"), "num_turns": result.get("num_turns"), "elapsed_s": result["elapsed_s"], "raw": result.get("raw")})
        print(f"[{LABEL}] task {index}: turns={result.get('num_turns')} error={result.get('is_error')} {result['elapsed_s']}s", flush=True)

    entries = []
    for file in (CONFIG_DIR / "projects").rglob(f"{session}.jsonl"):
        entries += [json.loads(line) for line in file.read_text().splitlines() if line.strip()]
    hits, tool_only_replies, tool_uses, prev = [], 0, 0, None
    for entry in entries:
        message = entry.get("message") or {}
        content = message.get("content")
        if message.get("role") == "user":
            kinds = {b.get("type") for b in content if isinstance(b, dict)} if isinstance(content, list) else {"text"}
            prev = "tool" if kinds == {"tool_result"} else "text"
            continue
        if message.get("role") != "assistant" or not isinstance(content, list):
            continue
        if prev == "tool":
            tool_only_replies += 1
        for block in content:
            if not isinstance(block, dict):
                continue
            if block.get("type") == "tool_use":
                tool_uses += 1
            text = block.get("text") if block.get("type") == "text" else block.get("thinking") if block.get("type") == "thinking" else None
            if not text:
                continue
            for match in LEAK.finditer(text):
                window = text[max(0, match.start() - 120): match.end() + 120]
                if CODE.search(window):
                    continue
                hits.append({"kind": block["type"], "after": prev, "match": match.group(0), "context": window.replace("\n", " ")})
    report = {
        "label": LABEL, "placeholder": PLACEHOLDER, "reminders_on_tool_turns": OLD_MODE == "1", "model": MODEL,
        "session": session, "turns": turns, "tool_uses": tool_uses, "tool_only_replies": tool_only_replies,
        "hits_text": [h for h in hits if h["kind"] == "text"], "hits_thinking": [h for h in hits if h["kind"] == "thinking"],
    }
    (ROOT / f"ab-{LABEL}.json").write_text(json.dumps(report, ensure_ascii=False, indent=2))
    print(f"[{LABEL}] tool_uses={tool_uses} tool_only_replies={tool_only_replies} hits text={len(report['hits_text'])} thinking={len(report['hits_thinking'])} errors={sum(1 for t in turns if t['error'])}", flush=True)


if __name__ == "__main__":
    main()
