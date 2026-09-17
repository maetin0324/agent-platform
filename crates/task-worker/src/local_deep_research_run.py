#!/usr/bin/env python3
"""Runner embedded in the taskd `local-deep-research` adapter (ADR-0029 D1,
extended by ADR-0031 D1 for the evidence record).

Contract with the adapter (crates/task-worker/src/local_deep_research.rs):
  argv[1]  path to a JSON file: {query, mode, settings, iterations,
           questions_per_iteration, report_path}
  stdout   one message per line: "progress: <text>" while running, and
           exactly one final line "TASKD_RESULT {json}" with
           {"summary": <=1500 chars, single line, "sources": <int>,
            "counts": {queries, search_results, sources, sources_cited,
                       unique_domains}}
  exit     0 on success, non-zero on failure (with a short message on stderr)

On success this also writes, next to `report_path` (i.e. in the same
`artifacts/` directory):
  sources.json    a de-duplicated-by-URL list [{url, title, engine, cited}]
  research.json   {queries: [{query, engine, result_count}], iterations,
                    counts: {queries, search_results, sources, sources_cited,
                             unique_domains}}
These are built mechanically from what the `local_deep_research` API
returned -- no LLM is involved (ADR-0031 D1: "LLM に書かせない"). The taskd
adapter reads `counts` straight off the TASKD_RESULT line to run the
evidence gate (ADR-0031 D2); it does not re-read research.json, but writes
it anyway for humans.

`search_results` is the number of raw source entries `local_deep_research`
returned *before* de-duplication by URL. The LDR API does not separately
expose a per-search-engine result count, so this is used as an acceptable
proxy for "how many hits did the search path return in total" (documented
here and in the Phase 21 implementation report per the task instructions).

Only the standard library and `local_deep_research` are used. The
`local_deep_research` import happens lazily inside main() so this module can
be imported on its own (e.g. to unit test `convert_setting_value` or
`build_evidence_manifest`) without the package installed.
"""

import json
import os
import re
import sys
from urllib.parse import urlparse


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


_CITATION_RE = re.compile(r"\[(\d+)\]")
_URL_IN_TEXT_RE = re.compile(r"\((https?://[^\s)]+)\)|(https?://[^\s)]+)")


def extract_cited_indices(text):
    """The 1-based `[n]` indices a summary cites (ADR-0031 D1: LDR cites
    sources by number into the `sources` list it returned)."""
    return {int(m) for m in _CITATION_RE.findall(text or "")}


def domain_of(url):
    """`netloc` without a leading `www.`, lower-cased. Empty string if `url`
    does not parse."""
    try:
        netloc = urlparse(url).netloc.lower()
    except ValueError:
        return ""
    if netloc.startswith("www."):
        netloc = netloc[4:]
    return netloc


def extract_queries(questions):
    """Flatten LDR's `questions` (a dict keyed by iteration, or a list -- the
    API does not document a single stable shape) into an ordered list of
    query strings."""
    queries = []
    if isinstance(questions, dict):
        def sort_key(k):
            try:
                return (0, int(k))
            except (TypeError, ValueError):
                return (1, str(k))

        for key in sorted(questions.keys(), key=sort_key):
            val = questions[key]
            if isinstance(val, list):
                queries.extend(str(q) for q in val if str(q).strip())
            elif val:
                queries.append(str(val))
    elif isinstance(questions, list):
        for val in questions:
            if isinstance(val, list):
                queries.extend(str(q) for q in val if str(q).strip())
            elif val:
                queries.append(str(val))
    return queries


def build_evidence_manifest(result):
    """Build the de-duplicated-by-URL sources list and the research manifest
    from an LDR API result dict (`quick_summary`/`detailed_research`, and
    `generate_report` on the rare occasion it returns the same shape).
    Mechanical only -- no LLM involved (ADR-0031 D1).

    Returns (sources_list, research) where `sources_list` is what gets
    written to `sources.json` and `research` is what gets written to
    `research.json` (see module docstring for the shapes).
    """
    raw_sources = result.get("sources") or []
    cited_indices = extract_cited_indices(str(result.get("summary") or ""))

    sources_list = []
    seen = {}
    for i, raw in enumerate(raw_sources, start=1):
        if isinstance(raw, dict):
            url = raw.get("link") or raw.get("url") or ""
            title = raw.get("title") or raw.get("name") or url or "source"
            # 実機（LDR 1.10.7）の 1 件は {id, index, link, snippet, source, title} で、
            # エンジン名は `source` に入る。
            engine = raw.get("engine") or raw.get("search_engine") or raw.get("source") or None
        else:
            url = str(raw)
            title = url
            engine = None
        if not url:
            continue
        cited_here = i in cited_indices
        if url in seen:
            idx = seen[url]
            if cited_here:
                sources_list[idx]["cited"] = True
            if not sources_list[idx].get("engine") and engine:
                sources_list[idx]["engine"] = engine
        else:
            seen[url] = len(sources_list)
            sources_list.append({"url": url, "title": title, "engine": engine, "cited": cited_here})

    queries = extract_queries(result.get("questions"))
    if not queries:
        # 実機（LDR 1.10.7 の quick_summary）では `questions` が空の dict で、実際に投げた問いは
        # `findings[].question` に入っていた。順序を保ったまま重複を落とす。
        seen_q = set()
        for finding in result.get("findings") or []:
            if not isinstance(finding, dict):
                continue
            question = str(finding.get("question") or "").strip()
            if question and question not in seen_q:
                seen_q.add(question)
                queries.append(question)
    # LDR's API does not expose a per-query engine or result count, only the
    # combined `sources` list, so those two fields are left null.
    query_manifest = [{"query": q, "engine": None, "result_count": None} for q in queries]

    domains = {d for d in (domain_of(s["url"]) for s in sources_list) if d}
    counts = {
        "queries": len(query_manifest),
        "search_results": len(raw_sources),
        "sources": len(sources_list),
        "sources_cited": sum(1 for s in sources_list if s["cited"]),
        "unique_domains": len(domains),
    }
    research = {
        "queries": query_manifest,
        "iterations": result.get("iterations") or 0,
        "counts": counts,
    }
    return sources_list, research


def build_evidence_manifest_from_text(text):
    """Fallback for `mode = "report"` when `generate_report` does not return
    a dict (its return shape is not documented by LDR). Sources are the URLs
    that appear in the rendered report text (de-duplicated); `cited` is not
    derivable from prose without `[n]` markers, `engine` is unknown, and
    there is no separate per-query breakdown, so `queries` stays empty.
    """
    urls = []
    for m in _URL_IN_TEXT_RE.finditer(text or ""):
        url = m.group(1) or m.group(2)
        if url:
            urls.append(url)

    sources_list = []
    seen = set()
    for url in urls:
        if url in seen:
            continue
        seen.add(url)
        sources_list.append({"url": url, "title": url, "engine": None, "cited": False})

    domains = {d for d in (domain_of(s["url"]) for s in sources_list) if d}
    counts = {
        "queries": 0,
        "search_results": len(urls),
        "sources": len(sources_list),
        "sources_cited": 0,
        "unique_domains": len(domains),
    }
    research = {"queries": [], "iterations": 0, "counts": counts}
    return sources_list, research


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
            summary, _ = write_report_from_result(report_path, query, result)
            sources_list, research = build_evidence_manifest(result)
        elif mode == "detailed":
            result = call_with_fallback(
                detailed_research, {"query": query, "settings_override": settings}, optional_kwargs
            )
            if not isinstance(result, dict):
                print(f"detailed_research returned an unexpected type: {type(result)!r}", file=sys.stderr)
                return 1
            summary, _ = write_report_from_result(report_path, query, result)
            sources_list, research = build_evidence_manifest(result)
        elif mode == "report":
            raw_result = call_with_fallback(
                generate_report,
                {"query": query, "settings_override": settings, "output_file": report_path},
                optional_kwargs,
            )
            text, _ = summarize_report_file(report_path)
            summary = text
            # `generate_report` is not documented to return the same shape as
            # `quick_summary`/`detailed_research`; use it if it happens to be
            # a dict, otherwise fall back to scraping URLs out of the report.
            if isinstance(raw_result, dict):
                sources_list, research = build_evidence_manifest(raw_result)
            else:
                sources_list, research = build_evidence_manifest_from_text(text)
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

    # ADR-0031 D1: write the evidence record next to report.md. Mechanical
    # (no LLM), so this happens even when the gate below (taskd-side, ADR-0031
    # D2) will later reject the run -- the files stay for a human to read.
    artifacts_dir = os.path.dirname(report_path) or "."
    with open(os.path.join(artifacts_dir, "sources.json"), "w", encoding="utf-8") as handle:
        json.dump(sources_list, handle, indent=2, ensure_ascii=False)
        handle.write("\n")
    with open(os.path.join(artifacts_dir, "research.json"), "w", encoding="utf-8") as handle:
        json.dump(research, handle, indent=2, ensure_ascii=False)
        handle.write("\n")

    print(
        "TASKD_RESULT "
        + json.dumps({"summary": summary_line, "sources": len(sources_list), "counts": research["counts"]})
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
