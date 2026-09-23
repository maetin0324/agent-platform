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

import html as html_module
import inspect
import json
import os
import re
import sys
import time
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
            # ADR-0063 Phase 109b B1: `add_must_read_sources` が付けた `primary: true` は
            # `sources.json`/report にも引き継ぐ（celeris のアダプタが「必読の一次情報のうち
            # 何件使われたか」を数えるのに読む）。
            primary = bool(raw.get("primary"))
        else:
            url = str(raw)
            title = url
            engine = None
            primary = False
        if not url:
            continue
        cited_here = i in cited_indices
        if url in seen:
            idx = seen[url]
            if cited_here:
                sources_list[idx]["cited"] = True
            if not sources_list[idx].get("engine") and engine:
                sources_list[idx]["engine"] = engine
            if primary:
                sources_list[idx]["primary"] = True
        else:
            seen[url] = len(sources_list)
            entry = {"url": url, "title": title, "engine": engine, "cited": cited_here}
            if primary:
                entry["primary"] = True
            sources_list.append(entry)

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


# --------------------------------------------- ADR-0063 Phase 109b B1: primary sources


MIN_CITED_EXCERPT_MATCH_CHARS = 24


def excerpt_is_cited(url, excerpt_text, body_text):
    """Whether a must-read primary source's excerpt shows up in the final
    report body (ADR-0063 Phase 109b B1): true if the URL itself is quoted
    in the body, or if some whitespace-collapsed line of the fetched
    excerpt (at least `MIN_CITED_EXCERPT_MATCH_CHARS` characters) appears
    verbatim in the whitespace-collapsed body. Best-effort and
    deterministic -- no LLM judges whether the source was "really" used."""
    body_collapsed = _collapse_whitespace(body_text or "").lower()
    if not body_collapsed:
        return False
    if url and str(url).lower() in body_collapsed:
        return True
    for line in str(excerpt_text or "").splitlines():
        candidate = _collapse_whitespace(line).lower()
        if len(candidate) >= MIN_CITED_EXCERPT_MATCH_CHARS and candidate in body_collapsed:
            return True
    return False


def apply_primary_source_citations(sources_list, research, primary_excerpts, body_text):
    """Whichever must-read primary source's excerpt shows up in the final
    report body counts as cited (ADR-0063 Phase 109b B1), even though LDR's
    own `[n]` citation markers can never point at an index appended after
    its own synthesis ran. `primary_excerpts` maps url -> excerpt text
    (only entries that were actually fetched with content). Mutates
    `sources_list` in place and recomputes `research["counts"]
    ["sources_cited"]` when something changed."""
    if not primary_excerpts:
        return
    changed = False
    for source in sources_list:
        if source.get("cited"):
            continue
        excerpt = primary_excerpts.get(source.get("url"))
        if excerpt and excerpt_is_cited(source.get("url"), excerpt, body_text):
            source["cited"] = True
            changed = True
    if changed:
        research["counts"]["sources_cited"] = sum(1 for s in sources_list if s.get("cited"))


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


_GITHUB_REPO_RE = re.compile(r"^https?://github\.com/([^/]+)/([^/]+?)(?:\.git)?/?$", re.IGNORECASE)
_GITLAB_REPO_RE = re.compile(r"^https?://gitlab\.com/([^/]+)/([^/]+?)(?:\.git)?/?$", re.IGNORECASE)
_SCRIPT_STYLE_RE = re.compile(r"(?is)<(script|style)[^>]*>.*?</\1>")
_TAG_RE = re.compile(r"(?s)<[^>]+>")
PRIMARY_EXCERPT_MAX_CHARS = 6000


def github_readme_url(url):
    """A bare GitHub/GitLab repository URL (`.../<owner>/<repo>`, no
    sub-path) -> its README's raw-text URL (ADR-0063 Phase 109b B1). `None`
    for anything else (a sub-path like `/issues/1`, or a non-repo host),
    which falls back to plain HTML fetching."""
    match = _GITHUB_REPO_RE.match(url.strip())
    if match:
        owner, repo = match.group(1), match.group(2)
        return f"https://raw.githubusercontent.com/{owner}/{repo}/HEAD/README.md"
    match = _GITLAB_REPO_RE.match(url.strip())
    if match:
        owner, repo = match.group(1), match.group(2)
        return f"https://gitlab.com/{owner}/{repo}/-/raw/HEAD/README.md"
    return None


def html_to_text(html):
    """HTML -> plain text (ADR-0063 Phase 109b B1): drop `<script>`/`<style>`
    blocks first (their content is not prose), strip every remaining tag,
    unescape entities, and collapse whitespace."""
    text = _SCRIPT_STYLE_RE.sub(" ", html or "")
    text = _TAG_RE.sub(" ", text)
    text = html_module.unescape(text)
    return _collapse_whitespace(text)


def default_primary_source_fetch(url, timeout=10):
    """The real network fetch for a must-read primary source (production
    only; tests inject a fake `fetch` instead)."""
    request = urllib.request.Request(url, headers={"User-Agent": "celeris-ldr/1.0"})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return response.read()


def build_primary_source_entries(must_read_urls, fetch=None, timeout=10):
    """Fetch each true `http(s)://` must-read URL once (ADR-0063 Phase 109b
    B1): a GitHub/GitLab repository URL's README (raw text, so no HTML
    stripping is needed), else the page's HTML converted to text. Returns
    `(section_markdown, entries)` where each entry is
    `{"link", "title", "source": "must-read", "primary": True, "excerpt",
    "fetch_error"}` (`excerpt` is `""` and `fetch_error` is set on failure).
    A `must_read_urls` entry that is not `http(s)://` is dropped (ADR-0063
    Phase 109b B1: `human` and the like from the knowledge base). `fetch(url,
    timeout) -> bytes` is injected so tests never touch the network; `None`
    resolves `default_primary_source_fetch` at call time (not bound as a
    default at import time), so a test can also monkeypatch that name and
    have `main()` -- which never passes its own `fetch` -- honor the fake."""
    if fetch is None:
        fetch = default_primary_source_fetch
    entries = []
    seen = set()
    for raw_url in must_read_urls or []:
        url = str(raw_url or "").strip()
        if not is_http_url(url) or url in seen:
            continue
        seen.add(url)
        readme_url = github_readme_url(url)
        fetch_target = readme_url or url
        title = url
        excerpt = ""
        error = None
        try:
            body = fetch(fetch_target, timeout)
        except Exception as exc:
            error = f"{type(exc).__name__}: {exc}"
        else:
            text = body.decode("utf-8", errors="replace") if isinstance(body, (bytes, bytearray)) else str(body)
            if readme_url:
                excerpt = text.strip()
            else:
                match = _TITLE_TAG_RE.search(text)
                if match:
                    title = _collapse_whitespace(match.group(1))
                excerpt = html_to_text(text)
            excerpt = excerpt[:PRIMARY_EXCERPT_MAX_CHARS]
        entries.append(
            {
                "link": url,
                "title": title,
                "source": "must-read",
                "primary": True,
                "excerpt": excerpt,
                "fetch_error": error,
            }
        )
    return render_primary_excerpts_section(entries), entries


def render_primary_excerpts_section(entries):
    """「## 必読の一次情報（本文抜粋）」節（ADR-0063 Phase 109b B1）。research question の直後に
    足され、LDR に渡す問いの一部になる。空リストなら空文字列（節そのものを付けない）。"""
    if not entries:
        return ""
    lines = ["## 必読の一次情報（本文抜粋）", ""]
    for entry in entries:
        lines.append(f"### {entry['title']} — {entry['link']}")
        if entry.get("fetch_error"):
            lines.append(f"(取得失敗: {entry['fetch_error']})")
        elif entry.get("excerpt"):
            lines.append(entry["excerpt"])
        else:
            lines.append("(本文なし)")
        lines.append("")
    return "\n".join(lines).rstrip("\n") + "\n"


def is_http_url(url):
    """`http(s)://` only (ADR-0063 Phase 109b B1: the knowledge base's
    `sources` occasionally carries a non-URL value like `"human"` -- a
    citation for a human-provided fact, not a page to read -- which must
    never be treated as a must-read source)."""
    return isinstance(url, str) and (url.startswith("http://") or url.startswith("https://"))


def add_must_read_sources(result, must_read_urls, fetch_title):
    """Append the must-read URLs (ADR-0063 D2/D3: 必読の一次情報) to
    `result["sources"]` in place, skipping any already present (matched by
    URL) or that are not `http(s)://` (ADR-0063 Phase 109b B1). Appended at
    the **end** so LDR's own `[n]` citation numbers in
    `summary`/`formatted_findings` keep pointing at the right entry.
    `fetch_title(url) -> str` is injected so tests never touch the network.
    Each appended entry carries `primary: True` (so `build_evidence_manifest`
    marks it in `sources.json`) and `cited: False` (the adapter/
    `apply_primary_source_citations` may flip it to `True` once the report
    body is known). Returns the number of URLs actually appended."""
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
        if url and is_http_url(url) and url not in existing:
            sources.append(
                {
                    "link": url,
                    "title": fetch_title(url),
                    "source": "must-read",
                    "primary": True,
                    "cited": False,
                }
            )
            existing.add(url)
            added += 1
    result["sources"] = sources
    return added


# ------------------------------------------------- ADR-0063 Phase 109b B2


def iteration_setting_overrides(func, iterations, questions_per_iteration):
    """Return `(settings_overrides, direct_kwargs)` for `iterations`/
    `questions_per_iteration` (ADR-0063 Phase 109b B2). Whichever of the two
    `func` actually declares as a named parameter (via `inspect.signature`)
    is passed directly as a kwarg -- so `call_with_fallback` can still drop
    it on a genuine `TypeError`; anything NOT declared goes into the LDR
    settings instead (`search.iterations` / `search.questions_per_iteration`),
    which `settings_override`/`settings_snapshot` reliably applies even when
    the function's own `**kwargs` would silently swallow an unnamed keyword.

    This is the real bug behind `research.json`'s `iterations` staying at
    LDR's settings-file default after Phase 109 set `retry_iterations = 5`:
    `detailed_research` only declares `query`/`settings_snapshot`/
    `progress_callback` (plus `**kwargs`), so the direct `iterations=5`
    kwarg landed in `**kwargs` and was silently never read."""
    try:
        declared = set(inspect.signature(func).parameters)
    except (TypeError, ValueError):
        declared = None  # cannot introspect; try both as direct kwargs (pre-Phase-109b behavior).

    overrides = {}
    direct = []
    for name, value, setting_key in (
        ("iterations", iterations, "search.iterations"),
        ("questions_per_iteration", questions_per_iteration, "search.questions_per_iteration"),
    ):
        if value is None:
            continue
        if declared is None or name in declared:
            direct.append((name, value))
        else:
            overrides[setting_key] = value
    return overrides, direct


# ------------------------------------------------- ADR-0063 Phase 109b B3


class UpstreamLlmError(Exception):
    """ADR-0063 Phase 109b B3: the LDR call itself raised, or returned a
    result whose synthesis text is actually an upstream LLM error (the
    llm-proxy's 503 written into `summary` as if it were the answer)."""


_UPSTREAM_ERROR_RE = re.compile(
    r"error code:\s*\d{3}\b|no_source_available|no reachable llm source",
    re.IGNORECASE,
)


def detect_upstream_llm_error(text):
    """Whether `text` (an LDR `summary`/body) is actually the *llm-proxy's*
    error message rather than a real answer (ADR-0063 Phase 109b B3: LDR's
    own synthesis step can swallow an LLM-call exception -- e.g. the
    llm-proxy's 503 `no_source_available` -- and write `str(exc)` into the
    `summary` field as if it were the answer, production observed
    2026-09-23). Returns a short description for the error message, or
    `None` if `text` does not look like a disguised upstream error."""
    if not _UPSTREAM_ERROR_RE.search(str(text or "")):
        return None
    return _collapse_whitespace(text)[:300]


LDR_RETRY_DELAYS = (2.0, 4.0, 8.0)


def call_ldr_stage(perform, extract_text, on_bad_result=None):
    """One attempt of an LDR API call (ADR-0063 Phase 109b B3). `perform()`
    does the actual call (and, for `generate_report`, the file write) and
    returns whatever the caller needs afterward; `extract_text(returned)`
    is the string to scan for a disguised upstream LLM error. A real
    exception, and a disguised error, both surface as `UpstreamLlmError` so
    `run_with_retries` treats them alike. `on_bad_result(returned)` runs
    before raising for a disguised error (e.g. to delete a `report.md` that
    `generate_report` already wrote with the bad content)."""
    try:
        returned = perform()
    except Exception as exc:
        raise UpstreamLlmError(f"{type(exc).__name__}: {exc}") from exc
    problem = detect_upstream_llm_error(extract_text(returned))
    if problem:
        if on_bad_result:
            on_bad_result(returned)
        raise UpstreamLlmError(f"llm error surfaced as the answer: {problem}")
    return returned


def run_with_retries(attempt_once, sleep=None, delays=LDR_RETRY_DELAYS):
    """Call `attempt_once()` up to `len(delays) + 1` times total (ADR-0063
    Phase 109b B3: 2s / 4s / 8s backoff between attempts, i.e. up to 3
    retries). Only `UpstreamLlmError` is retried; any other exception, or
    the last attempt's `UpstreamLlmError`, propagates. `sleep` is looked up
    from `time.sleep` at call time (not bound as a default at import time)
    so a test can monkeypatch `time.sleep` and have `main()` -- which never
    passes its own `sleep` -- honor the fake without any real waiting."""
    if sleep is None:
        sleep = time.sleep
    last_exc = None
    for delay in (*delays, None):
        try:
            return attempt_once()
        except UpstreamLlmError as exc:
            last_exc = exc
            if delay is None:
                raise
            sleep(delay)
    raise last_exc  # pragma: no cover -- the loop above always returns or raises


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

    try:
        os.makedirs(os.path.dirname(report_path) or ".", exist_ok=True)
    except OSError:
        pass

    # ADR-0063 Phase 109b B1: fetch each must-read primary source once, *before* any search, and
    # fold its excerpt into the question right after the research question itself. URLs that are not
    # `http(s)://` (e.g. `human` from the knowledge base) are dropped by `build_primary_source_entries`.
    primary_section = ""
    primary_entries = []
    if must_read:
        primary_section, primary_entries = build_primary_source_entries(must_read)
        for entry in primary_entries:
            if entry.get("fetch_error"):
                progress(f"must-read: {entry['link']} (fetch failed: {entry['fetch_error']})")
            else:
                progress(f"must-read: {entry['link']} ({len(entry.get('excerpt') or '')} chars)")
    augmented_query = f"{query}\n\n{primary_section}" if primary_section else query
    primary_titles = {e["link"]: e["title"] for e in primary_entries}
    primary_excerpts = {e["link"]: e["excerpt"] for e in primary_entries if e.get("excerpt")}

    def fetch_title_for(url):
        # Reuse the title already fetched while building the excerpt (no second request);
        # fall back to `fetch_url_title` only for a URL the excerpt stage did not see.
        return primary_titles.get(url) or fetch_url_title(url)

    try:
        if mode == "quick":
            overrides, direct = iteration_setting_overrides(quick_summary, iterations, questions_per_iteration)
            settings_q = dict(settings)
            settings_q.update(overrides)
            result = run_with_retries(
                lambda: call_ldr_stage(
                    lambda: call_with_fallback(
                        quick_summary,
                        {"query": augmented_query, "settings_override": settings_q},
                        [("progress_callback", progress)] + direct,
                    ),
                    lambda r: str((r or {}).get("summary") or "") if isinstance(r, dict) else "",
                )
            )
            if not isinstance(result, dict):
                print(f"quick_summary returned an unexpected type: {type(result)!r}", file=sys.stderr)
                return 1
            if must_read:
                add_must_read_sources(result, must_read, fetch_title_for)
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
            # uses it correctly. ADR-0063 Phase 109b B2: `detailed_research` also does not
            # declare `iterations`/`questions_per_iteration` as real parameters (they get
            # silently swallowed by its `**kwargs`), so those go into the settings snapshot
            # instead when `iteration_setting_overrides` finds them undeclared.
            overrides, direct = iteration_setting_overrides(detailed_research, iterations, questions_per_iteration)
            settings_d = dict(settings)
            settings_d.update(overrides)
            result = run_with_retries(
                lambda: call_ldr_stage(
                    lambda: call_with_fallback(
                        detailed_research,
                        {
                            "query": augmented_query,
                            "settings_snapshot": create_settings_snapshot(overrides=settings_d),
                        },
                        [("progress_callback", progress)] + direct,
                    ),
                    lambda r: str((r or {}).get("summary") or "") if isinstance(r, dict) else "",
                )
            )
            if not isinstance(result, dict):
                print(f"detailed_research returned an unexpected type: {type(result)!r}", file=sys.stderr)
                return 1
            if must_read:
                add_must_read_sources(result, must_read, fetch_title_for)
            summary, _ = write_report_from_result(report_path, query, result)
            sources_list, research = build_evidence_manifest(result)
        elif mode == "report":
            overrides, direct = iteration_setting_overrides(generate_report, iterations, questions_per_iteration)
            settings_r = dict(settings)
            settings_r.update(overrides)

            def do_report():
                raw = call_with_fallback(
                    generate_report,
                    {"query": augmented_query, "settings_override": settings_r, "output_file": report_path},
                    [("progress_callback", progress)] + direct,
                )
                text, _ = summarize_report_file(report_path)
                return raw, text

            def discard_report_file(_ignored):
                # ADR-0063 Phase 109b B3: `generate_report` already wrote `report_path` itself;
                # if its content is a disguised upstream error, remove it before retrying/giving up.
                try:
                    os.remove(report_path)
                except OSError:
                    pass

            raw_result, text = run_with_retries(
                lambda: call_ldr_stage(do_report, lambda rt: rt[1], on_bad_result=discard_report_file)
            )
            summary = text
            # `generate_report` is not documented to return the same shape as
            # `quick_summary`/`detailed_research`; use it if it happens to be
            # a dict, otherwise fall back to scraping URLs out of the report.
            if isinstance(raw_result, dict):
                if must_read:
                    add_must_read_sources(raw_result, must_read, fetch_title_for)
                sources_list, research = build_evidence_manifest(raw_result)
            else:
                sources_list, research = build_evidence_manifest_from_text(text)
        else:
            print(f"unknown mode: {mode}", file=sys.stderr)
            return 2

        # ADR-0063 Phase 109b B1: whichever must-read excerpt shows up in the report actually
        # written counts as cited, even though LDR's own `[n]` markers never point past its own
        # source list. Re-reads the file we (or `generate_report`) just wrote -- one extra read,
        # simplest way to check the same text a human/reviewer will see for every mode.
        if primary_excerpts:
            body_for_citation_check, _ = summarize_report_file(report_path)
            apply_primary_source_citations(sources_list, research, primary_excerpts, body_for_citation_check)
    except UpstreamLlmError as exc:
        # ADR-0063 Phase 109b B3: never leave a report.md behind that is actually the llm-proxy's
        # error message masquerading as an answer.
        try:
            if os.path.exists(report_path):
                os.remove(report_path)
        except OSError:
            pass
        print(f"llm: {exc}", file=sys.stderr)
        return 1
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
