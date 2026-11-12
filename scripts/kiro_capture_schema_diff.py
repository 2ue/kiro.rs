#!/usr/bin/env python3
"""Compare the upstream wire shape of two diagnostics capture archives.

Each argument is a capture archive (`/api/admin/diagnostics/capture/{id}/download`) or its
`events.jsonl`. Requests are grouped by phase and endpoint. The report separates client identity
(KiroIDE version), fixed SDK/API identifiers, host/path, header names and body field types.
Values are never printed except for UA identity fragments and hosts.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import zipfile
from collections import defaultdict
from pathlib import Path

IDE_RE = re.compile(r"KiroIDE-([0-9][0-9A-Za-z.-]*)-")
SDK_RE = re.compile(r"(aws-sdk-js/[0-9.]+|api/[\w-]+#[0-9.]+|md/nodejs#[0-9.]+|os/[\w#.-]+)")


def load_events(path: Path) -> list[dict]:
    if path.suffix == ".zip":
        with zipfile.ZipFile(path) as archive:
            text = archive.read("events.jsonl").decode()
    else:
        text = path.read_text()
    return [json.loads(line) for line in text.splitlines() if line.strip()]


def field_types(value, prefix: str, out: dict[str, set[str]]) -> None:
    if isinstance(value, dict):
        out.setdefault(prefix or "$", set()).add("object")
        for key, item in value.items():
            field_types(item, f"{prefix}.{key}" if prefix else key, out)
    elif isinstance(value, list):
        out.setdefault(prefix or "$", set()).add("array")
        for item in value:
            field_types(item, f"{prefix}[]", out)
    else:
        kind = "null" if value is None else type(value).__name__
        out.setdefault(prefix or "$", set()).add(kind)


def new_group() -> dict:
    return {
        "count": 0,
        "ide": set(),
        "sdk": set(),
        "hosts": set(),
        "paths": set(),
        "headers": set(),
        "fields": {},
        "statuses": set(),
    }


def summarize(events: list[dict]) -> dict[tuple[str, str], dict]:
    groups: dict[tuple[str, str], dict] = defaultdict(new_group)
    for event in events:
        data = event.get("data") or {}
        route = data.get("route") if isinstance(data.get("route"), dict) else {}
        key = (str(data.get("phase", "?")), str(route.get("endpoint", "?")))
        if event.get("type") == "upstream_response":
            groups[key]["statuses"].add(str(data.get("status")))
            continue
        if event.get("type") not in ("upstream_request", "protocol_request"):
            continue
        group = groups[key]
        group["count"] += 1
        headers = data.get("headers") or {}
        group["headers"].update(name.lower() for name in headers)
        for name in ("user-agent", "x-amz-user-agent"):
            agent = headers.get(name)
            if isinstance(agent, str):
                group["ide"].update(IDE_RE.findall(agent))
                group["sdk"].update(SDK_RE.findall(agent))
        url = str(data.get("url") or "")
        match = re.match(r"https?://([^/]+)(/[^?]*)?", url)
        if match:
            group["hosts"].add(headers.get("host") or match.group(1))
            group["paths"].add(match.group(2) or "/")
        body = data.get("body") or {}
        if isinstance(body, dict) and body.get("encoding") == "json":
            field_types(body.get("json"), "", group["fields"])
    return groups


def diff_sets(label: str, old: set, new: set) -> list[str]:
    if old == new:
        return []
    lines = [f"  {label}:"]
    lines += [f"    - {item}" for item in sorted(old - new)]
    lines += [f"    + {item}" for item in sorted(new - old)]
    return lines


def field_lines(group: dict) -> set[str]:
    return {f"{path}: {'|'.join(sorted(kinds))}" for path, kinds in group["fields"].items()}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("before", type=Path)
    parser.add_argument("after", type=Path)
    args = parser.parse_args()

    before = summarize(load_events(args.before))
    after = summarize(load_events(args.after))
    changed = False
    for key in sorted(set(before) | set(after)):
        old, new = before.get(key), after.get(key)
        title = f"[{key[0]} / {key[1]}]"
        if old is None or new is None:
            changed = True
            print(f"{title} only in {'after' if old is None else 'before'}")
            continue
        lines = (
            diff_sets("client identity (KiroIDE)", old["ide"], new["ide"])
            + diff_sets("fixed SDK/API identifiers", old["sdk"], new["sdk"])
            + diff_sets("host", old["hosts"], new["hosts"])
            + diff_sets("path", old["paths"], new["paths"])
            + diff_sets("header names", old["headers"], new["headers"])
            + diff_sets("body fields", field_lines(old), field_lines(new))
            + diff_sets("response status", old["statuses"], new["statuses"])
        )
        print(f"{title} requests before={old['count']} after={new['count']}")
        print("\n".join(lines) if lines else "  no wire-shape change")
        changed = changed or bool(lines)
    return 1 if changed else 0


if __name__ == "__main__":
    sys.exit(main())
