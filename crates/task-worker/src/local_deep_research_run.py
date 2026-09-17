#!/usr/bin/env python3
"""Runner embedded in the taskd `local-deep-research` adapter (ADR-0029 D1).

Contract with the adapter (crates/task-worker/src/local_deep_research.rs):
  argv[1]  path to a JSON file: {query, mode, settings, iterations,
           questions_per_iteration, report_path}
  stdout   one message per line: "progress: <text>" while running, and
           exactly one final line "TASKD_RESULT {json}" with
           {"summary": <=1500 chars, single line, "sources": <int>}
  exit     0 on success, non-zero on failure (with a short message on stderr)

Only the standard library and `local_deep_research` are used. The
`local_deep_research` import happens lazily inside main() so this module can
be imported on its own (e.g. to unit test `convert_setting_value`) without
the package installed.
"""

import json
import os
import sys


def convert_setting_value(value):
    """Convert a `[adapters.local_deep_research].settings` string value into a
    real Python type (ADR-0029 D1/D3: TOML values are written as strings so
    the types don't get mixed up on the Rust side; this converts them back).

    Order: bool -> JSON (arrays/objects, for values like
    `search.engine.web.searxng.default_params.engines`) -> int -> float ->
    left as the original string if none of the above apply.
    """
    if not isinstance(value, str):
        return value
    stripped = value.strip()
    lowered = stripped.lower()
    if lowered in ("true", "false"):
        return lowered == "true"
    if stripped[:1] in ("[", "{"):
        try:
            return json.loads(stripped)
        except (json.JSONDecodeError, ValueError):
            return value
    try:
        return int(stripped)
    except ValueError:
        pass
    try:
        return float(stripped)
    except ValueError:
        pass
    return value


def make_progress_printer():
    def progress(*args, **kwargs):
        text = None
        for arg in args:
            if isinstance(arg, str) and arg.strip():
                text = arg
                break
        if text is None:
            for key in ("message", "msg", "status", "log_message"):
                candidate = kwargs.get(key)
                if isinstance(candidate, str) and candidate.strip():
                    text = candidate
                    break
        if not text:
            return
        collapsed = " ".join(str(text).split())
        if collapsed:
            print(f"progress: {collapsed}", flush=True)

    return progress


def call_with_fallback(func, required_kwargs, optional_kwargs):
    """Call `func`, dropping one optional kwarg at a time on `TypeError` until
    the call succeeds or none are left (ADR-0029 D1: "wrap in try/except so
    an unsupported kwarg does not break the run" -- LDR's API functions don't
    all accept the same optional kwargs, e.g. `progress_callback`).
    """
    kwargs = dict(required_kwargs)
    remaining = list(optional_kwargs)
    kwargs.update({name: value for name, value in remaining})
    while True:
        try:
            return func(**kwargs)
        except TypeError:
            if not remaining:
                raise
            name, _ = remaining.pop(0)
            kwargs.pop(name, None)


def render_source(source):
    if isinstance(source, dict):
        url = source.get("url") or source.get("link") or ""
        title = source.get("title") or source.get("name") or url or "source"
        if url:
            return f"- [{title}]({url})"
        return f"- {title}"
    return f"- {source}"


def render_finding(finding):
    if isinstance(finding, dict):
        text = finding.get("finding") or finding.get("content") or finding.get("text")
        if text:
            return f"- {text}"
        return f"- {json.dumps(finding, ensure_ascii=False, default=str)}"
    return f"- {finding}"


def write_report_from_result(report_path, query, result):
    summary = str(result.get("summary") or "").strip()
    formatted_findings = result.get("formatted_findings")
    findings = result.get("findings") or []
    sources = result.get("sources") or []

    # 問いはアダプタが作った本文（タスクのタイトルを `# ...` の見出しとして含むことがある）。
    # そのまま `# {query}` にすると見出しが二重になるので、既に見出しで始まっていればそのまま使う。
    heading = query.strip() if query.strip().startswith("#") else f"# {query.strip()}"
    lines = [heading, "", "## Summary", "", summary or "(no summary)", "", "## Findings", ""]
    if isinstance(formatted_findings, str) and formatted_findings.strip():
        lines.append(formatted_findings.strip())
    elif findings:
        lines.extend(render_finding(f) for f in findings)
    else:
        lines.append("(no findings)")
    lines.extend(["", "## Sources", ""])
    if sources:
        lines.extend(render_source(s) for s in sources)
    else:
        lines.append("(no sources)")
    lines.append("")

    with open(report_path, "w", encoding="utf-8") as handle:
        handle.write("\n".join(lines))

    return summary, len(sources)


def summarize_report_file(report_path):
    try:
        with open(report_path, "r", encoding="utf-8") as handle:
            text = handle.read()
    except OSError:
        text = ""
    sources_count = sum(1 for line in text.splitlines() if "http://" in line or "https://" in line)
    return text, sources_count


def single_line(text, max_chars):
    collapsed = " ".join(str(text).split())
    if len(collapsed) > max_chars:
        collapsed = collapsed[:max_chars]
    return collapsed


def main():
    if len(sys.argv) < 2:
        print("usage: local_deep_research_run.py <input.json>", file=sys.stderr)
        return 2

    with open(sys.argv[1], "r", encoding="utf-8") as handle:
        payload = json.load(handle)

    query = payload["query"]
    mode = payload.get("mode", "quick")
    raw_settings = payload.get("settings") or {}
    settings = {key: convert_setting_value(value) for key, value in raw_settings.items()}
    iterations = payload.get("iterations")
    questions_per_iteration = payload.get("questions_per_iteration")
    report_path = payload["report_path"]

    try:
        from local_deep_research.api import detailed_research, generate_report, quick_summary
    except Exception as exc:  # pragma: no cover - exercised only with the real package installed
        print(f"failed to import local_deep_research: {exc}", file=sys.stderr)
        return 1

    progress = make_progress_printer()
    optional_kwargs = [("progress_callback", progress)]
    if questions_per_iteration is not None:
        optional_kwargs.append(("questions_per_iteration", questions_per_iteration))
    if iterations is not None:
        optional_kwargs.append(("iterations", iterations))

    try:
        os.makedirs(os.path.dirname(report_path) or ".", exist_ok=True)
    except OSError:
        pass

    try:
        if mode == "quick":
            result = call_with_fallback(quick_summary, {"query": query, "settings_override": settings}, optional_kwargs)
            if not isinstance(result, dict):
                print(f"quick_summary returned an unexpected type: {type(result)!r}", file=sys.stderr)
                return 1
            summary, sources_count = write_report_from_result(report_path, query, result)
        elif mode == "detailed":
            result = call_with_fallback(
                detailed_research, {"query": query, "settings_override": settings}, optional_kwargs
            )
            if not isinstance(result, dict):
                print(f"detailed_research returned an unexpected type: {type(result)!r}", file=sys.stderr)
                return 1
            summary, sources_count = write_report_from_result(report_path, query, result)
        elif mode == "report":
            call_with_fallback(
                generate_report,
                {"query": query, "settings_override": settings, "output_file": report_path},
                optional_kwargs,
            )
            text, sources_count = summarize_report_file(report_path)
            summary = text
        else:
            print(f"unknown mode: {mode}", file=sys.stderr)
            return 2
    except Exception as exc:
        print(f"local_deep_research call failed: {exc}", file=sys.stderr)
        return 1

    summary_line = single_line(summary, 1500)
    if not summary_line:
        print("local_deep_research produced no summary", file=sys.stderr)
        return 1

    print("TASKD_RESULT " + json.dumps({"summary": summary_line, "sources": sources_count}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
