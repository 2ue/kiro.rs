#!/usr/bin/env python3
"""关联结构化输出请求、抓包响应与 CCTest 分数，仅输出脱敏索引。"""

from __future__ import annotations

import argparse
import collections
import hashlib
import json
import re
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


SCHEMA_SHA256 = "93136f64e9d34e3d5bfdaee2e7d0d04173d03b435325748bea2dd9fbe13f77c0"
MESSAGES_SHA256 = "4ee3036002a9304a616fc15d085908446751e3282b658acefc09d684db2a23cb"
RESULT_STAMP = re.compile(r"^(20\d{6})T(\d{4})(\d{2})?")


def canonical_hash(value: Any) -> str:
    raw = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return hashlib.sha256(raw.encode("utf-8")).hexdigest()


def parse_result_time(path: Path) -> datetime | None:
    match = RESULT_STAMP.match(path.name)
    if not match:
        return None
    stamp = match.group(1) + match.group(2) + (match.group(3) or "00")
    return datetime.strptime(stamp, "%Y%m%d%H%M%S").replace(tzinfo=timezone.utc)


def parse_event_time(value: str) -> datetime:
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def sse_payloads(raw: str):
    for line in raw.splitlines():
        if not line.startswith("data: "):
            continue
        try:
            yield json.loads(line[6:])
        except json.JSONDecodeError:
            continue


def load_results(results_dir: Path) -> list[dict[str, Any]]:
    results = []
    for path in sorted(results_dir.glob("*.json")):
        result_time = parse_result_time(path)
        if result_time is None:
            continue
        try:
            payload = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        if payload.get("status") not in {"done", "error"}:
            continue
        results.append(
            {
                "path": path,
                "time": result_time,
                "status": payload.get("status"),
                "score": (payload.get("scores") or {}).get("output_config"),
            }
        )
    return results


def nearest_result(
    request_time: datetime, results: list[dict[str, Any]]
) -> tuple[dict[str, Any] | None, int | None, str]:
    candidates = [result for result in results if result["time"].date() == request_time.date()]
    ranked = sorted(candidates, key=lambda result: abs((request_time - result["time"]).total_seconds()))
    if not ranked:
        return None, None, "unmatched"
    delta = int((request_time - ranked[0]["time"]).total_seconds())
    if abs(delta) > 120:
        return None, None, "unmatched"
    if len(ranked) > 1:
        second_delta = abs((request_time - ranked[1]["time"]).total_seconds())
        first_delta = abs(delta)
        if second_delta - first_delta < 30:
            return None, None, "ambiguous"
    return ranked[0], delta, "matched"


def structured_requests(capture_dir: Path):
    event_path = capture_dir / "events.jsonl"
    requests: dict[str, dict[str, Any]] = {}
    text_parts: dict[str, list[str]] = collections.defaultdict(list)
    for raw_line in event_path.open("r", encoding="utf-8"):
        try:
            event = json.loads(raw_line)
        except json.JSONDecodeError:
            continue
        request_id = event.get("requestId")
        event_type = event.get("type")
        data = event.get("data") or {}
        if event_type == "client_request":
            body = ((data.get("rawBody") or {}).get("json") or {})
            format_schema = ((body.get("output_config") or {}).get("format") or {}).get("schema")
            if not isinstance(format_schema, dict) or canonical_hash(format_schema) != SCHEMA_SHA256:
                continue
            messages = body.get("messages")
            if not isinstance(messages, list) or canonical_hash(messages) != MESSAGES_SHA256:
                continue
            requests[request_id] = {
                "request_time": parse_event_time(event["ts"]),
                "model": body.get("model"),
                "stream": bool(body.get("stream")),
                "schema_sha256": SCHEMA_SHA256,
                "messages_sha256": MESSAGES_SHA256,
            }
        elif event_type == "client_sse" and request_id in requests:
            for payload in sse_payloads(str(data.get("text") or "")):
                if payload.get("type") != "content_block_delta":
                    continue
                delta = payload.get("delta") or {}
                if delta.get("type") == "text_delta":
                    text_parts[request_id].append(str(delta.get("text") or ""))

    for request_id, request in requests.items():
        response_text = "".join(text_parts.get(request_id, []))
        response_hash = hashlib.sha256(response_text.encode("utf-8")).hexdigest()
        response_object = None
        try:
            response_object = json.loads(response_text)
        except json.JSONDecodeError:
            pass
        valid_shape = (
            isinstance(response_object, dict)
            and set(response_object) == {"identity_platform", "desc"}
            and response_object.get("identity_platform") in {"kiro", "claude_code"}
            and isinstance(response_object.get("desc"), str)
        )
        request.update(
            {
                "request_id": request_id,
                "response_json_valid": response_object is not None,
                "response_schema_valid": bool(valid_shape),
                "identity_platform": response_object.get("identity_platform") if valid_shape else None,
                "desc_chars": len(response_object["desc"]) if valid_shape else None,
                "response_text_chars": len(response_text),
                "response_text_sha256": response_hash,
            }
        )
    return requests


def build_report(repo_root: Path) -> dict[str, Any]:
    capture_root = repo_root / "logs" / "llm-capture"
    results = load_results(repo_root / "docs/testing/cctest-kirotest/results")
    rows = []
    for capture_dir in sorted(capture_root.glob("capture-*")):
        if not (capture_dir / "events.jsonl").is_file():
            continue
        for request in structured_requests(capture_dir).values():
            result, delta, match_status = nearest_result(request["request_time"], results)
            result_status = result["status"] if result else None
            result_score = result["score"] if result else None
            status = (
                "complete"
                if result_status == "done" and isinstance(result_score, int)
                else "interrupted"
                if match_status == "matched"
                else match_status
            )
            rows.append(
                {
                    "capture": str((capture_dir / "events.jsonl").relative_to(repo_root)),
                    "request_id": request["request_id"],
                    "request_at_utc": request["request_time"].isoformat(),
                    "model": request["model"],
                    "stream": request["stream"],
                    "schema_sha256": request["schema_sha256"],
                    "messages_sha256": request["messages_sha256"],
                    "response_json_valid": request["response_json_valid"],
                    "response_schema_valid": request["response_schema_valid"],
                    "identity_platform": request["identity_platform"],
                    "desc_chars": request["desc_chars"],
                    "response_text_chars": request["response_text_chars"],
                    "response_text_sha256": request["response_text_sha256"],
                    "result": str(result["path"].relative_to(repo_root)) if result else None,
                    "result_status": result_status,
                    "result_match_delta_seconds": delta,
                    "output_config_score": result_score,
                    "pair_status": status,
                }
            )
    rows.sort(key=lambda row: (row["request_at_utc"], row["request_id"]))

    complete = [row for row in rows if row["pair_status"] == "complete"]
    identities = collections.Counter(row["identity_platform"] for row in complete)
    scores_by_identity = collections.defaultdict(collections.Counter)
    scores_by_model = collections.defaultdict(
        lambda: collections.defaultdict(collections.Counter)
    )
    for row in complete:
        scores_by_identity[row["identity_platform"]][str(row["output_config_score"])] += 1
        scores_by_model[row["model"]][row["identity_platform"]][
            str(row["output_config_score"])
        ] += 1
    summary = {
        "captured_requests": len(rows),
        "complete_pairs": len(complete),
        "interrupted_pairs": len(rows) - len(complete),
        "complete_by_identity": dict(sorted(identities.items())),
        "scores_by_identity": {
            identity: dict(sorted(scores.items()))
            for identity, scores in sorted(scores_by_identity.items())
        },
        "scores_by_model_and_identity": {
            model: {
                identity: dict(sorted(scores.items()))
                for identity, scores in sorted(identities_for_model.items())
            }
            for model, identities_for_model in sorted(scores_by_model.items())
        },
    }
    return {
        "description": "脱敏的 output_config 样本关联清单；不包含请求正文、响应正文、凭证或账号信息。",
        "schema_sha256": SCHEMA_SHA256,
        "messages_sha256": MESSAGES_SHA256,
        "summary": summary,
        "samples": rows,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    default_root = Path(__file__).resolve().parents[4]
    parser.add_argument("--repo-root", type=Path, default=default_root)
    parser.add_argument(
        "--output",
        type=Path,
        default=default_root / "docs/testing/cctest-kirotest/analysis/output-config-samples.json",
    )
    parser.add_argument(
        "--check-snapshot",
        action="store_true",
        help="校验当前归档快照为 46 个请求、44 个完整配对及 10/34 分数矩阵。",
    )
    args = parser.parse_args()
    report = build_report(args.repo_root.resolve())
    if args.check_snapshot:
        summary = report["summary"]
        expected = {
            "captured_requests": 46,
            "complete_pairs": 44,
            "interrupted_pairs": 2,
            "complete_by_identity": {"claude_code": 10, "kiro": 34},
            "scores_by_identity": {"claude_code": {"10": 10}, "kiro": {"0": 34}},
            "scores_by_model_and_identity": {
                "claude-opus-4-8": {"claude_code": {"10": 6}},
                "claude-opus-5-5": {
                    "claude_code": {"10": 4},
                    "kiro": {"0": 34},
                },
            },
        }
        if summary != expected:
            raise SystemExit(f"快照与预期不符：{json.dumps(summary, ensure_ascii=False)}")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report["summary"], ensure_ascii=False, sort_keys=True))


if __name__ == "__main__":
    main()
