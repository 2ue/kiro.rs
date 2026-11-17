#!/usr/bin/env python3
"""Combine scheduler and open-loop verdicts; missing cases are failures."""
import json
import sys
from pathlib import Path

out = Path(sys.argv[1])
rows = ["# Claude Pool 验收矩阵", "", "| 用例 | 阶段 | 状态分布 | ok 率 | p95 (ms) | stall (s) | RSS MB / FD（开始→峰值→结束） | PASS/FAIL |",
        "|---|---|---|---|---|---|---|---|"]
failed = False
for name in sys.argv[2:]:
    path = out / f"{name}.json"
    try:
        report = json.loads(path.read_text())
    except (OSError, ValueError):
        report = {"verdict": "FAIL", "problems": ["报告缺失或无效"]}
    verdict = report.get("verdict", "FAIL")
    failed |= verdict == "FAIL"
    resource = report.get("resources")
    resources = "—"
    if resource:
        resources = " → ".join(f"{resource[key]['rssMb']:.1f}/{resource[key]['fd']}"
                               if resource[key].get('rssMb') is not None else "采样失败"
                               for key in ("start", "peak", "end"))
    for phase in report.get("phases") or [{}]:
        status = phase.get("byStatus") or dict(ok=phase.get("ok", 0), rejected=phase.get("rejected", 0), **phase.get("otherKinds", {}))
        total = phase.get("total", phase.get("attempts", 0))
        rate = phase.get("okRate", round(100 * phase.get("ok", 0) / total, 1) if total else 0)
        p95 = phase.get("p95", phase.get("okP95Max", phase.get("okP95")))
        rows.append(f"| {name} | {phase.get('phase', phase.get('segment', '—'))} | `{json.dumps(status)}` | {rate}% | {p95} | {phase.get('stallSecs', 0)} | {resources} | {'FAIL' if verdict == 'FAIL' else phase.get('verdict', verdict)} |")
    for problem in report.get("problems", []):
        rows.extend(["", f"- **{name}**: {problem}"])
rows.extend(["", "总结果：**FAIL**" if failed else "总结果：**PASS**", ""])
(out / "SUMMARY.md").write_text("\n".join(rows))
sys.exit(1 if failed else 0)
