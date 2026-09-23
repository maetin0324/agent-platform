#!/usr/bin/env python3
"""Runner embedded in the celeris `local-deep-research` adapter (ADR-0029 D1,
extended by ADR-0031 D1 for the evidence record).

Extended by ADR-0063 D2/D3/D4 (Phase 109): a `must_read_urls` field of primary
sources that get force-merged into `sources`/report.md/sources.json after LDR
answers (`add_must_read_sources`), and a `"<env:NAME>"` placeholder scheme so
a secret `settings` value (an `api_key`, say) never has to be written into
`ldr_input.json` in the clear (`resolve_env_placeholder`).

Contract with the adapter (crates/task-worker/src/local_deep_research.rs):
  argv[1]  path to a JSON file: {query, mode, settings, iterations,
           questions_per_iteration, report_path, must_read_urls}
  stdout   one message per line: "progress: <text>" while running, and
           exactly one final line "CELERIS_RESULT {json}" with
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
returned -- no LLM is involved (ADR-0031 D1: "LLM に書かせない"). The celeris
adapter reads `counts` straight off the CELERIS_RESULT line to run the
evidence gate (ADR-0031 D2); it does not re-read research.json, but writes
it anyway for humans.

`search_results` is the number of raw source entries `local_deep_research`
returned *before* de-duplication by URL. The LDR API does not separately
expose a per-search-engine result count, so this is used as an acceptable
proxy for "how many hits did the search path return in total" (documented
here and in the Phase 21 implementation report per the task instructions).

`report.md` (`write_report_from_result`) is built mechanically too -- no LLM
involved. A real run (Phase 32) showed LDR can return `summary` and
`formatted_findings` that are the same synthesis text 2-3 times over (plus an
inflated, repeated-URL source list); the reviewer correctly rejected that.
So the body is de-duplicated (`build_report_body`: `summary` /
`formatted_findings` / `findings[].content` that are substantially the same
text collapse into the single longest one, written once, with no
`## Summary`/`## Findings` section headings), the heading comes from LDR's
own `#`-prefixed synthesis when present (else the first sentence of the
objective, not the whole objective), and `## 出典` de-duplicates by URL while
keeping the original citation numbers (`[n] (= [m])` for a repeat) so `[n]`
references in the body still point at the right line.

Only the standard library and `local_deep_research` are used. The
`local_deep_research` import happens lazily inside main() so this module can
be imported on its own (e.g. to unit test `convert_setting_value` or
`build_evidence_manifest`) without the package installed.
"""

import json
import os
import re
import sys
import urllib.request
from urllib.parse import urlparse

_ENV_PLACEHOLDER_RE = re.compile(r"^<env:([A-Za-z_][A-Za-z0-9_]*)>$")


def resolve_env_placeholder(value):
    """`"<env:LDR_LLM_OPENAI_ENDPOINT_API_KEY>"` -> that environment
    variable's value, or `""` if it is unset. Any other string is returned
    unchanged.

    ADR-0063 D4 (Phase 109): the celeris adapter never writes a secret
    setting value (a key ending in `api_key`/`token`/`password`/`secret`)
    into `ldr_input.json` in the clear -- only this placeholder. The real
    value reaches this process as an environment variable of the child
    process (the Rust side derives the same `LDR_...` name and sets it via
    `.envs(...)`)."""
    if not isinstance(value, str):
        return value
    match = _ENV_PLACEHOLDER_RE.match(value.strip())
    if not match:
        return value
    return os.environ.get(match.group(1), "")


def convert_setting_value(value):
    """Convert a `[adapters.local_deep_research].settings` string value into a
    real Python type (ADR-0029 D1/D3: TOML values are written as strings so
    the types don't get mixed up on the Rust side; this converts them back).

    A `"<env:...>"` placeholder (ADR-0063 D4) is resolved from the
    environment first; everything below only applies to what is left.

    Order: bool -> JSON (arrays/objects, for values like
    `search.engine.web.searxng.default_params.engines`) -> int -> float ->
    left as the original string if none of the above apply.
    """
    if not isinstance(value, str):
        return value
    stripped = resolve_env_placeholder(value.strip())
    if not isinstance(stripped, str):
        return stripped
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
    # Not `value`: a resolved `"<env:...>"` placeholder must survive even when it is not a
    # bool/JSON/int/float (ADR-0063 D4). For every other input `stripped == value.strip()`,
    # so this is the same behavior as before Phase 109.
    return stripped


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


def _collapse_whitespace(text):
    return " ".join(str(text).split())


def objective_title_sentence(text, max_chars=80):
    """タスクの objective（`query`）から題名の材料を取る（実機のレビュー不合格の是正:
    以前は objective 全文を `# ` に足していたため、1477 行の報告の 1 行目が objective 丸ごとに
    なっていた）。空白をたたんだ上で、最初の「。」までの 1 文（無ければ全体）を、最大
    `max_chars` 字に切り詰める。
    """
    collapsed = _collapse_whitespace(text)
    period = collapsed.find("。")
    sentence = collapsed[: period + 1] if period != -1 else collapsed
    if len(sentence) > max_chars:
        sentence = sentence[:max_chars]
    return sentence


def _body_candidates(result):
    """本文になり得る候補（`summary`、`formatted_findings`、`findings[].content`）を、書かれた順に
    集める。空・非文字列は除く。
    """
    candidates = []
    summary = str(result.get("summary") or "").strip()
    if summary:
        candidates.append(summary)
    formatted_findings = result.get("formatted_findings")
    if isinstance(formatted_findings, str):
        formatted_findings = formatted_findings.strip()
        if formatted_findings:
            candidates.append(formatted_findings)
    for finding in result.get("findings") or []:
        if not isinstance(finding, dict):
            continue
        text = finding.get("finding") or finding.get("content") or finding.get("text")
        if isinstance(text, str):
            text = text.strip()
            if text:
                candidates.append(text)
    return candidates


def _is_same_body(a, b, prefix_chars=200):
    """2 つの本文候補が「実質同じ」か（先頭 `prefix_chars` 字が一致、または片方が他方を含む）。"""
    if not a or not b:
        return False
    if a == b or a in b or b in a:
        return True
    return len(a) >= prefix_chars and len(b) >= prefix_chars and a[:prefix_chars] == b[:prefix_chars]


def build_report_body(result):
    """`report.md` の本文を組み立てる（実機のレビュー不合格の是正。ADR-0029 D1 追記）。
    `summary` / `formatted_findings` / `findings[].content` のうち実質同じもの
    （`_is_same_body`）は最も長い 1 つにまとめ、1 回だけ書く。`## Summary` / `## Findings` /
    `## Final synthesis` のような区画見出しは付けない。反復ログ（`iterations` /
    `findings[].question` の羅列）はここでは扱わない（`research.json` にある。ADR-0031 D1）。
    純粋に文字列の集約・重複排除だけで、LLM は呼ばない。
    """
    groups = []
    for candidate in _body_candidates(result):
        merged = False
        for i, kept in enumerate(groups):
            if _is_same_body(candidate, kept):
                if len(candidate) > len(kept):
                    groups[i] = candidate
                merged = True
                break
        if not merged:
            groups.append(candidate)
    return "\n\n".join(groups)


def render_sources_section(sources):
    """`## 出典` の中身。`sources` の元の順（= LDR の引用番号順）を保ったまま、URL の重複行は
    `[n] (= [m])` に畳む（本文中の `[n]` 参照を書き換えるのは安全にできないため。ADR-0029 D1 追記）。
    """
    lines = []
    first_index_for_url = {}
    for i, raw in enumerate(sources, start=1):
        if isinstance(raw, dict):
            url = raw.get("link") or raw.get("url") or ""
            title = raw.get("title") or raw.get("name") or url or "source"
        else:
            url = str(raw)
            title = url
        if not url:
            continue
        if url in first_index_for_url:
            lines.append(f"[{i}] (= [{first_index_for_url[url]}])")
        else:
            first_index_for_url[url] = i
            lines.append(f"[{i}] {title} — {url}")
    if not lines:
        lines.append("(no sources)")
    return lines


def write_report_from_result(report_path, query, result):
    """`report.md` を書く。題名は次の優先順で決める（実機のレビュー不合格の是正):
      1. `query`（アダプタが渡す問い）が既に `#` 見出しで始まっていればそれを使う
         （そのまま `# {query}` にすると見出しが二重になるので、これまでどおり避ける）。
      2. 本文（`build_report_body`）が `#` 見出しで始まっていればそれを題名として使い、
         別途見出しは足さない（LDR 自身が付けた題名を優先する）。
      3. どちらでもなければ、objective（`query`）の先頭 1 文を `# ` にする
         （以前は objective 全文を見出しにしていたため 1 行目が肥大化していた）。
    本文は `build_report_body` が返す 1 回分だけ。出典は `render_sources_section` が
    引用番号を保ったまま URL で重複排除する。
    """
    body = build_report_body(result)
    raw_summary = str(result.get("summary") or "").strip()
    q = (query or "").strip()

    if q.startswith("#"):
        heading = q.splitlines()[0].strip()
        main_section = f"{heading}\n\n{body or '(no summary)'}"
    elif body.startswith("#"):
        main_section = body
    else:
        heading = f"# {objective_title_sentence(q)}"
        main_section = f"{heading}\n\n{body or '(no summary)'}"

    sources = result.get("sources") or []
    lines = [main_section, "", "## 出典", ""]
    lines.extend(render_sources_section(sources))
    lines.append("")

    with open(report_path, "w", encoding="utf-8") as handle:
        handle.write("\n".join(lines))

    return raw_summary or body, len(sources)


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


_TITLE_TAG_RE = re.compile(r"<title[^>]*>([^<]{1,200})</title>", re.IGNORECASE)


def fetch_url_title(url, timeout=10):
    """Best-effort `<title>` for a must-read URL (ADR-0063 D3). Any problem
    (network, timeout, non-HTML response, no `<title>`) just falls back to
    the URL itself -- this is a courtesy for a human reading sources.json /
    report.md, never a hard requirement (LDR's own search is what must
    succeed)."""
    try:
        request = urllib.request.Request(url, headers={"User-Agent": "celeris-ldr/1.0"})
        with urllib.request.urlopen(request, timeout=timeout) as response:
            body = response.read(65536)
        match = _TITLE_TAG_RE.search(body.decode("utf-8", errors="replace"))
        if match:
            return _collapse_whitespace(match.group(1))
    except Exception:
        pass
    return url


def add_must_read_sources(result, must_read_urls, fetch_title):
    """Append the must-read URLs (ADR-0063 D2/D3: 必読の一次情報) to
    `result["sources"]` in place, skipping any already present (matched by
    URL). Appended at the **end** so LDR's own `[n]` citation numbers in
    `summary`/`formatted_findings` keep pointing at the right entry.
    `fetch_title(url) -> str` is injected so tests never touch the network.
    Returns the number of URLs actually appended."""
    sources = list(result.get("sources") or [])
    existing = set()
    for raw in sources:
        if isinstance(raw, dict):
            url = raw.get("link") or raw.get("url")
        else:
            url = str(raw)
        if url:
            existing.add(url)
    added = 0
    for url in must_read_urls or []:
        url = str(url or "").strip()
        if url and url not in existing:
            sources.append({"link": url, "title": fetch_title(url), "source": "must-read"})
            existing.add(url)
            added += 1
    result["sources"] = sources
    return added


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
    # ADR-0063 D2/D3 (Phase 109): must-read primary sources (URLs the adapter pulled out of the
    # task's objective/inputs and the knowledge base). Forced into `sources`/report.md/sources.json
    # after LDR answers -- LDR's own search is unaffected (this is not a search engine).
    must_read = [u for u in (payload.get("must_read_urls") or []) if str(u or "").strip()]

    try:
        from local_deep_research.api import (
            create_settings_snapshot,
            detailed_research,
            generate_report,
            quick_summary,
        )
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
            if must_read:
                add_must_read_sources(result, must_read, fetch_url_title)
            summary, _ = write_report_from_result(report_path, query, result)
            sources_list, research = build_evidence_manifest(result)
        elif mode == "detailed":
            # `detailed_research` has no `settings_override`/`settings` parameter of its
            # own (unlike `quick_summary`/`generate_report`): it only honours a
            # `settings_snapshot` kwarg, and otherwise falls back to
            # `create_settings_snapshot()` with no overrides, silently ignoring any
            # `settings_override` passed in `**kwargs`. Confirmed against the real
            # package (ADR-0029): calling with `settings_override=settings` raises
            # "Ollama model not configured" even when `settings` has `llm.model` set,
            # while `settings_snapshot=create_settings_snapshot(overrides=settings)`
            # uses it correctly.
            result = call_with_fallback(
                detailed_research,
                {"query": query, "settings_snapshot": create_settings_snapshot(overrides=settings)},
                optional_kwargs,
            )
            if not isinstance(result, dict):
                print(f"detailed_research returned an unexpected type: {type(result)!r}", file=sys.stderr)
                return 1
            if must_read:
                add_must_read_sources(result, must_read, fetch_url_title)
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
                if must_read:
                    add_must_read_sources(raw_result, must_read, fetch_url_title)
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
    # (no LLM), so this happens even when the gate below (celeris-side, ADR-0031
    # D2) will later reject the run -- the files stay for a human to read.
    artifacts_dir = os.path.dirname(report_path) or "."
    with open(os.path.join(artifacts_dir, "sources.json"), "w", encoding="utf-8") as handle:
        json.dump(sources_list, handle, indent=2, ensure_ascii=False)
        handle.write("\n")
    with open(os.path.join(artifacts_dir, "research.json"), "w", encoding="utf-8") as handle:
        json.dump(research, handle, indent=2, ensure_ascii=False)
        handle.write("\n")

    print(
        "CELERIS_RESULT "
        + json.dumps({"summary": summary_line, "sources": len(sources_list), "counts": research["counts"]})
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
