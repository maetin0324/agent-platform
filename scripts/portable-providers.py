#!/usr/bin/env python3
"""Remove vendor pins from general-purpose harnesses; preserve unrelated TOML verbatim.

Default: report changes only, without printing configuration values or secrets.
--apply: save a private backup and atomically replace the configuration.
"""

import argparse
import os
from pathlib import Path
import re
import tempfile
import tomllib


PORTABLE = {"conversation", "secretary", "coding", "data-analysis", "writing", "plan", "reviewer"}
AGENTS = {"claude-code", "codex", "acp"}


def migrate(text):
    before = tomllib.loads(text)
    changes = []
    # A section ends at any TOML table header, including nested tables.
    parts = re.split(r"(?m)(?=^\s*\[)", text)
    for index, part in enumerate(parts):
        if not re.match(r"^\s*\[\[(harnesses|roles)\]\]", part):
            continue
        section = tomllib.loads(part)
        kind = next(iter(section))
        item = section[kind][0]
        if item.get("id") not in PORTABLE or item.get("adapter") not in AGENTS:
            continue
        updated, count = re.subn(r"(?m)^\s*adapter\s*=.*(?:\n|$)", "", part)
        if count != 1:
            raise ValueError("adapter must be a separate TOML assignment")
        parts[index] = updated
        changes.append(f"{kind}.{item['id']}: adapter pin -> automatic")
    result = "".join(parts)
    after = tomllib.loads(result)
    # Ensure that only the intended adapter fields changed, even with unusual formatting.
    for kind in ("harnesses", "roles"):
        for item in before.get(kind, []):
            if item.get("id") in PORTABLE and item.get("adapter") in AGENTS:
                del item["adapter"]
    if before != after:
        raise ValueError("migration changed unexpected settings")
    return result, changes


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("config", type=Path)
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    path = args.config.expanduser().resolve(strict=True)
    original = path.read_text()
    updated, changes = migrate(original)
    for change in changes:
        print(change)
    if not changes:
        print("No changes needed.")
        return
    if not args.apply:
        print("Preview only. Use --apply after updating Celeris (ADR-0049).")
        return
    fd, backup = tempfile.mkstemp(prefix=path.name + ".before-portable-", dir=path.parent)
    with os.fdopen(fd, "w") as output:
        output.write(original)
    fd, temporary = tempfile.mkstemp(prefix=path.name + ".portable-", dir=path.parent)
    try:
        with os.fdopen(fd, "w") as output:
            output.write(updated)
            output.flush()
            os.fsync(output.fileno())
        if path.read_text() != original:
            raise RuntimeError("configuration changed during migration; refusing to overwrite")
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    print(f"Updated {path}; private backup: {backup}")
    print("Restart Celeris to load harness changes. Existing tasks retain their explicit adapter pins.")


if __name__ == "__main__":
    main()
