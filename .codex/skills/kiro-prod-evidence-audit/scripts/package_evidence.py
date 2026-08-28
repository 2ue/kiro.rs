#!/usr/bin/env python3
"""Redact and package a kiro.rs production evidence directory.

Default archive excludes raw/ and includes README.md, commands.md, manifest.json,
summary/, problems/, and redacted/.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import tarfile
import time
from typing import Iterable


TEXT_EXTENSIONS = {
    ".txt",
    ".log",
    ".md",
    ".json",
    ".jsonl",
    ".yaml",
    ".yml",
    ".toml",
    ".env",
    ".conf",
    ".ini",
    ".sql",
    ".csv",
}

SECRET_PATTERNS: list[tuple[re.Pattern[str], str]] = [
    (
        re.compile(
            r"(?im)(\b[A-Za-z_][A-Za-z0-9_]*(?:PASSWORD|PASSWD|SECRET|TOKEN|"
            r"API[_-]?KEY|ACCESS[_-]?KEY|PRIVATE[_-]?KEY)\b\s*[:=]\s*)"
            r"([^\s#\"'{}]+)"
        ),
        r"\1[REDACTED]",
    ),
    (
        re.compile(
            r"(?i)(authorization[\"']?\s*[:=]\s*[\"']?\s*)"
            r"(bearer|basic)\s+([A-Za-z0-9._~+/=-]{12,})"
        ),
        r"\1\2 [REDACTED]",
    ),
    (
        re.compile(
            r"(?i)\b(password|passwd|pwd|secret|token|api[_-]?key|client[_-]?secret|"
            r"refresh[_-]?token|access[_-]?token|session[_-]?id|cookie)\b"
            r"(\s*[=:]\s*|\"\s*:\s*\")"
            r"([^\"'\s,;}]+)"
        ),
        r"\1\2[REDACTED]",
    ),
    (
        re.compile(r"(?i)(postgres(?:ql)?://)([^:@/\s]+):([^@/\s]+)@"),
        r"\1[REDACTED_USER]:[REDACTED_PASS]@",
    ),
    (
        re.compile(r"(?i)(redis://)(:|[^:@/\s]+:)?([^@/\s]*)@"),
        r"\1[REDACTED]@",
    ),
    (
        re.compile(r"(?i)([a-z][a-z0-9+.-]*://)([^:@/\s]+):([^@/\s]+)@"),
        r"\1[REDACTED_USER]:[REDACTED_PASS]@",
    ),
    (
        re.compile(r"\b(AKIA|ASIA)[A-Z0-9]{16}\b"),
        "[REDACTED_AWS_KEY]",
    ),
    (
        re.compile(r"\bsk-[A-Za-z0-9_-]{20,}\b"),
        "[REDACTED_API_KEY]",
    ),
    (
        re.compile(r"\beyJ[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b"),
        "[REDACTED_JWT]",
    ),
    (
        re.compile(r"\b[A-Za-z0-9._%+-]{2,}@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b"),
        "[REDACTED_EMAIL]",
    ),
    (
        re.compile(r"(?<!\d)(?:\d{1,3}\.){3}\d{1,3}(?!\d)"),
        "[REDACTED_IP]",
    ),
]

SENSITIVE_JSON_KEYS = {
    "authorization",
    "password",
    "passwd",
    "pwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "client_secret",
    "refresh_token",
    "access_token",
    "session_id",
    "cookie",
    "request_api_key_id",
    "credential_label",
    "credential_email",
}


def is_shareable_path(rel: Path) -> bool:
    """Keep high-risk raw-derived tables out of the default shareable archive.

    The raw captures remain available locally for audit, while the problem folders contain
    deliberately minimized evidence suitable for sharing outside the production session.
    """

    if not rel.parts or rel.parts[0] != "redacted":
        return True
    if len(rel.parts) >= 2 and rel.parts[1] in {"host", "docker"}:
        return False
    if len(rel.parts) >= 3 and rel.parts[1] == "app" and rel.parts[2] == "process-env.txt":
        return False
    if len(rel.parts) >= 3 and rel.parts[1] == "db":
        name = rel.parts[2]
        if (
            name.startswith("request-")
            or name in {"request-record.txt", "thinking-errors.txt", "latest-usage.txt", "schema.txt"}
        ):
            return False
    return True


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def is_text_file(path: Path) -> bool:
    if path.suffix.lower() in TEXT_EXTENSIONS:
        return True
    try:
        sample = path.read_bytes()[:4096]
    except OSError:
        return False
    if b"\x00" in sample:
        return False
    try:
        sample.decode("utf-8")
        return True
    except UnicodeDecodeError:
        return False


def redact_text(text: str) -> str:
    redacted = text
    for pattern, replacement in SECRET_PATTERNS:
        redacted = pattern.sub(replacement, redacted)
    return redacted


def scrub_sensitive_json_value(value: object) -> object:
    """Remove known high-risk request body payloads from structured diagnostics."""

    if isinstance(value, dict):
        scrubbed: dict[str, object] = {}
        for key, child in value.items():
            normalized_key = re.sub(r"[^a-z0-9]+", "_", key.lower()).strip("_")
            if normalized_key in SENSITIVE_JSON_KEYS:
                scrubbed[key] = "[REDACTED]"
                continue
            if key == "requestBody" and isinstance(child, dict):
                request_body = dict(child)
                if "content" in request_body:
                    request_body["content"] = "[REDACTED_REQUEST_BODY_CONTENT]"
                scrubbed[key] = scrub_sensitive_json_value(request_body)
                continue

            if key == "bytes" and isinstance(child, str):
                scrubbed[key] = "[REDACTED_BYTES]"
                continue

            if key == "content" and isinstance(child, str) and len(child) > 4096:
                scrubbed[key] = "[REDACTED_LARGE_CONTENT]"
                continue

            scrubbed[key] = scrub_sensitive_json_value(child)
        return scrubbed

    if isinstance(value, list):
        return [scrub_sensitive_json_value(item) for item in value]

    return value


def redact_structured_text(text: str, source_name: str) -> str:
    """Apply text redaction plus JSON/JSONL-aware payload scrubbing."""

    if source_name.endswith(".jsonl"):
        lines: list[str] = []
        for line in text.splitlines():
            if line.startswith("{"):
                try:
                    parsed = json.loads(line)
                except json.JSONDecodeError:
                    lines.append(redact_text(line))
                    continue
                scrubbed = scrub_sensitive_json_value(parsed)
                lines.append(redact_text(json.dumps(scrubbed, ensure_ascii=False)))
            else:
                lines.append(redact_text(line))
        return "\n".join(lines) + ("\n" if text.endswith("\n") else "")

    if source_name.endswith(".json"):
        try:
            parsed = json.loads(text)
        except json.JSONDecodeError:
            return redact_text(text)
        scrubbed = scrub_sensitive_json_value(parsed)
        return redact_text(json.dumps(scrubbed, ensure_ascii=False, indent=2) + "\n")

    return redact_text(text)


def iter_files(root: Path) -> Iterable[Path]:
    for path in sorted(root.rglob("*")):
        if path.is_file() and not path.is_symlink():
            yield path


def source_date_epoch() -> int:
    raw = os.environ.get("SOURCE_DATE_EPOCH")
    if raw is None:
        return int(time.time())
    try:
        return max(0, int(raw))
    except ValueError as error:
        raise SystemExit("SOURCE_DATE_EPOCH must be a non-negative integer") from error


def write_redacted(raw_dir: Path, redacted_dir: Path, max_file_bytes: int) -> list[dict[str, object]]:
    entries: list[dict[str, object]] = []
    if not raw_dir.exists():
        return entries

    for source in iter_files(raw_dir):
        rel = source.relative_to(raw_dir)
        target = redacted_dir / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        size = source.stat().st_size
        entry: dict[str, object] = {
            "path": str(Path("raw") / rel),
            "size": size,
            "sha256": sha256_file(source),
            "redacted_path": str(Path("redacted") / rel),
        }

        if size > max_file_bytes:
            target.write_text(
                f"[SKIPPED: file is {size} bytes, above redaction limit {max_file_bytes}]\n",
                encoding="utf-8",
            )
            entry["redaction"] = "skipped_size_limit"
        elif is_text_file(source):
            text = source.read_text(encoding="utf-8", errors="replace")
            target.write_text(redact_structured_text(text, source.name), encoding="utf-8")
            entry["redaction"] = "text_redacted"
        else:
            target.write_text(
                f"[SKIPPED: non-text evidence file, raw sha256={entry['sha256']}]\n",
                encoding="utf-8",
            )
            entry["redaction"] = "skipped_non_text"

        entries.append(entry)
    return entries


def write_default_files(root: Path) -> None:
    readme = root / "README.md"
    if not readme.exists():
        readme.write_text(
            "# Production evidence archive\n\n"
            "Generated by kiro-prod-evidence-audit. Default archive excludes raw evidence.\n",
            encoding="utf-8",
        )
    commands = root / "commands.md"
    if not commands.exists():
        commands.write_text(
            "# Commands\n\n"
            "Record production read-only commands here.\n",
            encoding="utf-8",
        )
    for subdir in ("summary", "problems", "redacted"):
        (root / subdir).mkdir(parents=True, exist_ok=True)


def build_manifest(
    root: Path,
    raw_entries: list[dict[str, object]],
    include_raw: bool,
    generated_at_epoch: int,
) -> dict[str, object]:
    manifest: dict[str, object] = {
        "generated_at_epoch": generated_at_epoch,
        "root": str(root),
        "include_raw_in_archive": include_raw,
        "raw_files": raw_entries,
        "packaged_files": [],
    }
    packaged: list[dict[str, object]] = []
    for path in iter_files(root):
        rel = path.relative_to(root)
        if rel.parts and rel.parts[0] == "raw" and not include_raw:
            continue
        if not is_shareable_path(rel):
            continue
        if rel == Path("manifest.json"):
            continue
        if path.name.endswith(".tar.gz"):
            continue
        packaged.append(
            {
                "path": str(rel),
                "size": path.stat().st_size,
                "sha256": sha256_file(path),
            }
        )
    manifest["packaged_files"] = packaged
    return manifest


def create_archive(
    root: Path,
    archive: Path,
    include_raw: bool,
    generated_at_epoch: int,
) -> None:
    archive.parent.mkdir(parents=True, exist_ok=True)
    with archive.open("wb") as raw_archive:
        with gzip.GzipFile(fileobj=raw_archive, mode="wb", filename="", mtime=generated_at_epoch) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as tar:
                for path in iter_files(root):
                    rel = path.relative_to(root)
                    if rel.parts and rel.parts[0] == "raw" and not include_raw:
                        continue
                    if not is_shareable_path(rel):
                        continue
                    if path.name.endswith(".tar.gz") or path.resolve() == archive.resolve():
                        continue

                    def normalize(info: tarfile.TarInfo) -> tarfile.TarInfo:
                        info.uid = 0
                        info.gid = 0
                        info.uname = ""
                        info.gname = ""
                        info.mtime = generated_at_epoch
                        return info

                    tar.add(path, arcname=str(Path(root.name) / rel), filter=normalize)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, help="Evidence root directory")
    parser.add_argument(
        "--archive",
        help="Output .tar.gz path. Defaults to <root>/<root-name>-redacted.tar.gz",
    )
    parser.add_argument(
        "--include-raw",
        action="store_true",
        help="Include raw/ in archive. Requires explicit user approval before use.",
    )
    parser.add_argument(
        "--max-file-mb",
        type=int,
        default=25,
        help="Maximum individual raw text file size to redact in memory",
    )
    args = parser.parse_args()

    root = Path(args.root).expanduser().resolve()
    if not root.exists() or not root.is_dir():
        raise SystemExit(f"Evidence root does not exist or is not a directory: {root}")

    write_default_files(root)
    raw_entries = write_redacted(root / "raw", root / "redacted", args.max_file_mb * 1024 * 1024)
    generated_at_epoch = source_date_epoch()
    manifest = build_manifest(root, raw_entries, args.include_raw, generated_at_epoch)
    manifest_path = root / "manifest.json"
    manifest_path.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    archive = (
        Path(args.archive).expanduser().resolve()
        if args.archive
        else root / f"{root.name}-{'raw' if args.include_raw else 'redacted'}.tar.gz"
    )
    create_archive(root, archive, args.include_raw, generated_at_epoch)
    print(json.dumps({"archive": str(archive), "include_raw": args.include_raw}, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
