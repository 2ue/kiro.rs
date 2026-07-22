#!/usr/bin/env python3
"""Dependency-free structural validation for this repository-owned skill."""

from __future__ import annotations

import re
import sys
from pathlib import Path


MAX_SKILL_NAME_LENGTH = 64
ALLOWED_FRONTMATTER_KEYS = {
    "name",
    "description",
    "license",
    "allowed-tools",
    "metadata",
}
NAME_RE = re.compile(r"^[a-z0-9-]+$")
FRONTMATTER_RE = re.compile(r"^---\r?\n(.*?)\r?\n---(?:\r?\n|$)", re.DOTALL)


class ValidationError(ValueError):
    pass


def parse_plain_frontmatter(content: str) -> dict[str, str]:
    match = FRONTMATTER_RE.match(content)
    if not match:
        raise ValidationError("SKILL.md must start with a closed YAML frontmatter block")

    values: dict[str, str] = {}
    for line_number, raw_line in enumerate(match.group(1).splitlines(), start=2):
        if not raw_line.strip() or raw_line.lstrip().startswith("#"):
            continue
        if raw_line[:1].isspace():
            raise ValidationError(
                f"frontmatter line {line_number} uses nested YAML; "
                "the dependency-free validator accepts top-level scalar fields only"
            )
        key, separator, raw_value = raw_line.partition(":")
        if not separator:
            raise ValidationError(f"frontmatter line {line_number} is missing ':'")
        key = key.strip()
        if key in values:
            raise ValidationError(f"duplicate frontmatter key: {key}")
        if key not in ALLOWED_FRONTMATTER_KEYS:
            allowed = ", ".join(sorted(ALLOWED_FRONTMATTER_KEYS))
            raise ValidationError(f"unexpected frontmatter key {key!r}; allowed: {allowed}")
        value = raw_value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in {'"', "'"}:
            value = value[1:-1]
        values[key] = value
    return values


def parse_agents_interface(path: Path) -> dict[str, str]:
    if not path.is_file():
        raise ValidationError("agents/openai.yaml not found")
    values: dict[str, str] = {}
    in_interface = False
    for line_number, raw_line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        if not raw_line.strip() or raw_line.lstrip().startswith("#"):
            continue
        if raw_line == "interface:":
            in_interface = True
            continue
        if not in_interface:
            raise ValidationError(
                f"agents/openai.yaml line {line_number} must be inside interface"
            )
        if not raw_line.startswith("  ") or raw_line.startswith("    "):
            raise ValidationError(
                f"agents/openai.yaml line {line_number} has unsupported nesting"
            )
        key, separator, raw_value = raw_line.strip().partition(":")
        if not separator:
            raise ValidationError(f"agents/openai.yaml line {line_number} is missing ':'")
        value = raw_value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in {'"', "'"}:
            value = value[1:-1]
        values[key] = value
    return values


def validate_skill(skill_path: Path) -> None:
    if not skill_path.is_dir():
        raise ValidationError(f"skill directory not found: {skill_path}")
    skill_md = skill_path / "SKILL.md"
    if not skill_md.is_file():
        raise ValidationError("SKILL.md not found")

    frontmatter = parse_plain_frontmatter(skill_md.read_text(encoding="utf-8"))
    name = frontmatter.get("name", "").strip()
    description = frontmatter.get("description", "").strip()
    if not name:
        raise ValidationError("missing non-empty 'name' in frontmatter")
    if not description:
        raise ValidationError("missing non-empty 'description' in frontmatter")
    if not NAME_RE.fullmatch(name):
        raise ValidationError("name must contain only lowercase letters, digits, and hyphens")
    if name.startswith("-") or name.endswith("-") or "--" in name:
        raise ValidationError("name cannot start/end with a hyphen or contain consecutive hyphens")
    if len(name) > MAX_SKILL_NAME_LENGTH:
        raise ValidationError(
            f"name is {len(name)} characters; maximum is {MAX_SKILL_NAME_LENGTH}"
        )
    if skill_path.name != name:
        raise ValidationError(
            f"skill directory {skill_path.name!r} does not match frontmatter name {name!r}"
        )
    if "<" in description or ">" in description:
        raise ValidationError("description cannot contain angle brackets")
    if len(description) > 1024:
        raise ValidationError("description exceeds 1024 characters")

    interface = parse_agents_interface(skill_path / "agents" / "openai.yaml")
    for key in ("display_name", "short_description", "default_prompt"):
        if not interface.get(key, "").strip():
            raise ValidationError(f"agents/openai.yaml is missing interface.{key}")
    if f"${name}" not in interface["default_prompt"]:
        raise ValidationError("agents default_prompt must reference the skill by its $name")

    for relative in (
        "references/evidence-map.md",
        "references/kiro-rs-evidence-sources.md",
        "scripts/package_evidence.py",
    ):
        path = skill_path / relative
        if not path.is_file() or path.stat().st_size == 0:
            raise ValidationError(f"required non-empty skill resource missing: {relative}")


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print("Usage: python3 quick_validate.py <skill_directory>", file=sys.stderr)
        return 2
    try:
        validate_skill(Path(argv[1]).resolve())
    except (OSError, UnicodeError, ValidationError) as error:
        print(f"Skill validation failed: {error}", file=sys.stderr)
        return 1
    print("Skill is valid (dependency-free repository validator).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
