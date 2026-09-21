#!/usr/bin/env bash
# scripts/selfdeploy/status.sh — ADR-0040 D2。いまの current / previous、リリース一覧（gate と verify の
# 要約）、本番 celeris（127.0.0.1:7710）と GUI（127.0.0.1:7700）の health、`daemon_instances`（D4）を
# JSON 1 つで出す。**読むだけ**（誰が実行してもよい）。
set -euo pipefail

SD_PROG=status
# shellcheck source=lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

case "${1:-}" in
  -h | --help)
    cat >&2 <<'EOF'
usage: status.sh

  JSON を標準出力に出す。何も変えない。
    current / previous / releases[] (gate.json / verify.json / promoted.json / changes.json の要約と
    on_main) / health / gui_health / daemon_instances
EOF
    exit 2
    ;;
esac

sd_require_json_tool
[ "$SD_JSON_TOOL" = python3 ] || sd_die "status.sh needs python3"

# `daemon_instances` は Phase 47 で入る表。無くても落ちない。
DAEMON_ROWS='null'
if [ -r "$SD_DB" ]; then
  if out="$(sqlite3 "file:$SD_DB?mode=ro" -json \
    'SELECT instance_id, release, pid, role, started_at, heartbeat_at, handoff_requested_at, drained_at
       FROM daemon_instances ORDER BY started_at' 2>/dev/null)"; then
    DAEMON_ROWS="${out:-[]}"
  fi
fi

HEALTH_JSON='null'
if [ "$(sd_http_status "$SD_PROD_API/api/v1/health")" = 200 ]; then
  HEALTH_JSON="$(sd_http_get "$SD_PROD_API/api/v1/health" || echo null)"
fi
GUI_HEALTH_JSON='null'
if [ "$(sd_http_status "http://127.0.0.1:$SD_PROD_GUI_PORT/healthz")" = 200 ]; then
  GUI_HEALTH_JSON="$(sd_http_get "http://127.0.0.1:$SD_PROD_GUI_PORT/healthz" || echo null)"
fi

export SD_RELEASES SD_CURRENT SD_PREVIOUS CELERIS_CONFIG_DIR CELERIS_STATE_DIR SD_BACKUPS
export SD_STATUS_AT="$(sd_ts)"
export SD_STATUS_HEALTH="$HEALTH_JSON"
export SD_STATUS_GUI_HEALTH="$GUI_HEALTH_JSON"
export SD_STATUS_DAEMON="$DAEMON_ROWS"
export SD_STATUS_CURRENT="$(sd_current_sha)"
export SD_STATUS_PREVIOUS="$(sd_previous_sha)"

# ADR-0041 D3: `main` に反映されているか。作業チェックアウト（`$SD_REPO`）が git リポジトリで
# `main` を持つときだけ見る。無ければ `on_main` は全部 `null`（`GET /releases` と同じ扱い）。
# **読むだけ**（`git` は `merge-base --is-ancestor` しか使わない。checkout も fetch もしない）。
SD_STATUS_GIT_REPO=""
if git -C "$SD_REPO" rev-parse --verify --quiet main >/dev/null 2>&1; then
  SD_STATUS_GIT_REPO="$SD_REPO"
fi
export SD_STATUS_GIT_REPO

python3 <<'PY'
import json, os, subprocess, sys

releases_dir = os.environ["SD_RELEASES"]
git_repo = os.environ.get("SD_STATUS_GIT_REPO") or ""


def on_main(sha):
    """その sha が `main` の祖先か（ADR-0041 D3）。分からなければ None。"""
    if not git_repo or not sha:
        return None
    try:
        done = subprocess.run(
            ["git", "-C", git_repo, "merge-base", "--is-ancestor", sha, "main"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5, check=False,
        )
    except Exception:  # noqa: BLE001
        return None
    if done.returncode == 0:
        return True
    if done.returncode == 1:
        return False
    return None  # その sha がこのリポジトリに無い等
current = os.environ.get("SD_STATUS_CURRENT") or None
previous = os.environ.get("SD_STATUS_PREVIOUS") or None


def load(path):
    try:
        with open(path, encoding="utf-8") as fh:
            return json.load(fh)
    except Exception:  # noqa: BLE001
        return None


def parse_env_json(name):
    raw = (os.environ.get(name) or "").strip()
    if not raw or raw == "null":
        return None
    try:
        return json.loads(raw)
    except Exception:  # noqa: BLE001
        return None


items = []
if os.path.isdir(releases_dir):
    for name in sorted(os.listdir(releases_dir)):
        if name.startswith("."):
            continue
        d = os.path.join(releases_dir, name)
        if not os.path.isdir(d):
            continue
        manifest = load(os.path.join(d, "manifest.json")) or {}
        gate = load(os.path.join(d, "gate.json")) or {}
        verify = load(os.path.join(d, "verify.json"))
        promoted = load(os.path.join(d, "promoted.json"))
        promote_failed = load(os.path.join(d, "promote_failed.json"))
        changes = load(os.path.join(d, "changes.json"))
        items.append({
            "promoted_at": (promoted or {}).get("promoted_at"),
            "promoted": promoted,
            "promote_failed": promote_failed,
            # ADR-0041 D3: 本番に出た版が `main` に戻っているか（null = 分からない）。
            "on_main": on_main(manifest.get("sha") or name),
            # ADR-0041 D4: 昇格したら何が変わるか（`release.sh` がビルド時に書いた要約）。
            "changes": None if changes is None else {
                "base": changes.get("base"),
                "stale": changes.get("base") != current,
                "commit_count": len(changes.get("commits") or []),
                "file_count": len(changes.get("files") or []),
                "sensitive": changes.get("sensitive") or [],
            },
            "sha12": name,
            "ref": manifest.get("ref"),
            "built_at": manifest.get("built_at"),
            "schema_version": manifest.get("schema_version"),
            "celeris_version": manifest.get("celeris_version"),
            "gate": {
                "ok": bool(gate.get("ok")),
                "failed_step": gate.get("failed_step") or None,
                "steps": [{"step": s.get("step"), "exit": s.get("exit"), "secs": s.get("secs")}
                          for s in (gate.get("steps") or [])],
            },
            "verify": None if verify is None else {
                "ok": bool(verify.get("ok")),
                "live_ok": bool(verify.get("live_ok")),
                "at": verify.get("at"),
                "failed": [c.get("name") for c in (verify.get("checks") or []) if not c.get("ok")],
            },
            "is_current": name == current,
            "is_previous": name == previous,
            "has_bin": os.path.isfile(os.path.join(d, "bin", "celeris")),
        })

items.sort(key=lambda i: (i["built_at"] or ""), reverse=True)

out = {
    "at": os.environ.get("SD_STATUS_AT"),
    "config_dir": os.environ.get("CELERIS_CONFIG_DIR"),
    "state_dir": os.environ.get("CELERIS_STATE_DIR"),
    "current": current,
    "previous": previous,
    "health": parse_env_json("SD_STATUS_HEALTH"),
    "gui_health": parse_env_json("SD_STATUS_GUI_HEALTH"),
    "daemon_instances": parse_env_json("SD_STATUS_DAEMON"),
    "releases": items,
    "backups": sorted(
        (f for f in os.listdir(os.environ["SD_BACKUPS"]) if f.endswith(".sqlite3"))
        if os.path.isdir(os.environ["SD_BACKUPS"]) else [],
        reverse=True,
    )[:10],
}
json.dump(out, sys.stdout, ensure_ascii=False, indent=2)
sys.stdout.write("\n")
PY
