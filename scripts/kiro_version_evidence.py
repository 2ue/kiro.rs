#!/usr/bin/env python3
"""Extract Kiro IDE and streaming SDK versions from an official macOS bundle."""

from __future__ import annotations

import argparse
import hashlib
import json
import plistlib
import re
import sys
import tempfile
import urllib.request
import zipfile
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import urlparse


BASE = "https://prod.download.desktop.kiro.dev"
PACKAGE_RE = re.compile(r"^@(?:aws|amzn)/[\w.-]*(?:codewhisperer|kiro)[\w.-]*$")
VERSION_RE = re.compile(r"^\d+(?:\.\d+){2,}(?:-[0-9A-Za-z.-]+)?$")
PACKAGE_LITERAL_RE = re.compile(
    r'"?name"?\s*:\s*"(@(?:aws|amzn)/[\w.-]*(?:codewhisperer|kiro)[\w.-]*)"'
    r'\s*,\s*"?description"?\s*:\s*"[^"]*"'
    r'\s*,\s*"?version"?\s*:\s*"([0-9][^"]*)"'
)
MINIFIED_PACKAGE_RE = re.compile(
    r'[\w$]+="(@(?:aws|amzn)/[\w.-]*(?:codewhisperer|kiro)[\w.-]*)",'
    r'[\w$]+="[^"]*",[\w$]+="([0-9][^"]*)"'
)
STREAMING_PACKAGE = "@aws/codewhisperer-streaming-client"


def fetch(url: str) -> tuple[bytes, dict[str, str], str]:
    request = urllib.request.Request(url, headers={"User-Agent": "kiro-version-evidence/1.0"})
    with urllib.request.urlopen(request, timeout=60) as response:
        final_url = response.geturl()
        if urlparse(final_url).hostname != "prod.download.desktop.kiro.dev":
            raise RuntimeError(f"official download redirected to unexpected host: {final_url}")
        body = response.read()
        headers = {key.lower(): value for key, value in response.headers.items()}
        return body, headers, final_url


def download(url: str, destination: Path) -> tuple[str, dict[str, str], str, int]:
    request = urllib.request.Request(url, headers={"User-Agent": "kiro-version-evidence/1.0"})
    digest = hashlib.sha256()
    size = 0
    with urllib.request.urlopen(request, timeout=60) as response:
        final_url = response.geturl()
        if urlparse(final_url).hostname != "prod.download.desktop.kiro.dev":
            raise RuntimeError(f"official download redirected to unexpected host: {final_url}")
        headers = {key.lower(): value for key, value in response.headers.items()}
        with destination.open("wb") as output:
            while chunk := response.read(1024 * 1024):
                output.write(chunk)
                digest.update(chunk)
                size += len(chunk)
    return digest.hexdigest(), headers, final_url, size


def version_key(value: str) -> tuple[tuple[int, object], ...]:
    parts = re.split(r"[.-]", value)
    return tuple((0, int(part)) if part.isdigit() else (1, part) for part in parts)


def add_package(packages: dict[str, set[str]], name: str, version: str) -> None:
    if PACKAGE_RE.fullmatch(name) and VERSION_RE.fullmatch(version):
        packages.setdefault(name, set()).add(version)


def package_versions(bundle: zipfile.ZipFile, extension_root: str) -> dict[str, list[str]]:
    packages: dict[str, set[str]] = {}
    prefix = extension_root.rstrip("/") + "/"
    for member in bundle.infolist():
        if not member.filename.startswith(prefix) or member.is_dir():
            continue
        filename = member.filename
        if filename.endswith("package.json"):
            try:
                package = json.loads(bundle.read(member))
            except (UnicodeDecodeError, json.JSONDecodeError):
                continue
            if isinstance(package, dict):
                add_package(packages, str(package.get("name", "")), str(package.get("version", "")))
        elif filename.endswith((".js", ".cjs", ".mjs")):
            source = bundle.read(member).decode("utf-8", errors="ignore")
            for match in PACKAGE_LITERAL_RE.finditer(source):
                add_package(packages, match.group(1), match.group(2))
            for match in MINIFIED_PACKAGE_RE.finditer(source):
                add_package(packages, match.group(1), match.group(2))
    return {name: sorted(values, key=version_key) for name, values in sorted(packages.items())}


def inspect_installer(archive_path: Path, requested_version: str) -> dict[str, object]:
    with zipfile.ZipFile(archive_path) as bundle:
        corrupt_member = bundle.testzip()
        app_prefixes = sorted(
            {
                member.filename.split("Contents/Info.plist", 1)[0]
                for member in bundle.infolist()
                if member.filename.endswith(".app/Contents/Info.plist")
            }
        )
        if not app_prefixes:
            raise RuntimeError("installer ZIP has no .app/Contents/Info.plist")
        app_prefix = app_prefixes[0]
        plist_path = f"{app_prefix}Contents/Info.plist"
        info = plistlib.loads(bundle.read(plist_path))
        bundle_version = str(info.get("CFBundleShortVersionString", ""))

        app_resources = f"{app_prefix}Contents/Resources/app"
        extension_root = f"{app_resources}/extensions/kiro.kiro-agent"
        has_extension = any(
            member.filename.startswith(extension_root.rstrip("/") + "/")
            for member in bundle.infolist()
        )
        if not has_extension:
            extension_root = app_resources
        packages = package_versions(bundle, extension_root)
        streaming_sdks = packages.get(STREAMING_PACKAGE, [])

    checks = {
        "zip_crc": "passed" if corrupt_member is None else f"failed:{corrupt_member}",
        "bundle_version_matches_requested": bundle_version == requested_version,
        "streaming_sdk_found": bool(streaming_sdks),
    }
    return {
        "bundle_version": bundle_version,
        "extension_root": extension_root,
        "packages": packages,
        "streaming_sdk_versions": streaming_sdks,
        "recommended_streaming_sdk_version": streaming_sdks[-1] if streaming_sdks else None,
        "checks": checks,
        "validation": "passed"
        if all(value is True or value == "passed" for value in checks.values())
        else "failed",
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform", choices=("darwin-arm64", "darwin-x64"), default="darwin-arm64")
    parser.add_argument("--version", help="specific Kiro version; defaults to official currentRelease")
    parser.add_argument("--output", type=Path, help="write evidence JSON to this path instead of stdout")
    args = parser.parse_args()

    metadata_url = f"{BASE}/stable/metadata-{args.platform}-stable.json"
    metadata_bytes, metadata_headers, metadata_final_url = fetch(metadata_url)
    metadata = json.loads(metadata_bytes)
    current_release = str(metadata["currentRelease"])
    requested_version = args.version or current_release
    if not VERSION_RE.fullmatch(requested_version):
        raise RuntimeError(f"invalid Kiro version: {requested_version}")

    archive_name = f"kiro-ide-{requested_version}-stable-{args.platform}.zip"
    archive_url = f"{BASE}/releases/stable/{args.platform}/signed/{requested_version}/{archive_name}"
    with tempfile.TemporaryDirectory(prefix="kiro-version-evidence-") as temporary_directory:
        archive_path = Path(temporary_directory) / archive_name
        digest, archive_headers, archive_final_url, archive_size = download(archive_url, archive_path)
        extracted = inspect_installer(archive_path, requested_version)

    evidence = {
        "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "source": {
            "metadata_url": metadata_url,
            "metadata_final_url": metadata_final_url,
            "metadata_etag": metadata_headers.get("etag"),
            "official_current_release": current_release,
            "installer_url": archive_url,
            "installer_final_url": archive_final_url,
            "installer_etag": archive_headers.get("etag"),
            "installer_last_modified": archive_headers.get("last-modified"),
        },
        "installer": {
            "requested_version": requested_version,
            "platform": args.platform,
            "size_bytes": archive_size,
            "sha256_observed": digest,
            "sha256_note": "computed locally; no official checksum comparison was available",
        },
        "extracted": extracted,
    }
    rendered = json.dumps(evidence, indent=2, sort_keys=True) + "\n"
    if args.output:
        args.output.write_text(rendered, encoding="utf-8")
    else:
        sys.stdout.write(rendered)
    return 0 if extracted["validation"] == "passed" else 1


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (KeyError, OSError, RuntimeError, urllib.error.URLError, zipfile.BadZipFile) as error:
        print(f"kiro version evidence failed: {error}", file=sys.stderr)
        raise SystemExit(1)
