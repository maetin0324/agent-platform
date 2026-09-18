#!/usr/bin/env python3
"""Runner embedded in the taskd `paperqa` adapter (ADR-0035 D1 / D5).

Before PaperQA2 can answer anything it needs papers. This runner is the
"go and find the papers" stage:

  0. **the search terms are written by the LLM** (ADR-0035 D5, Phase 36).
     One `chat/completions` call to the same OpenAI-compatible endpoint
     PaperQA2 itself uses (`OPENAI_BASE_URL`), asking for 3-6 English
     queries plus the terms that mark an off-topic paper. If anything
     about that answer is broken, the deterministic terms the adapter
     extracted (`queries` in the input) are used instead and the
     fallback is announced on `progress:`.
  1. search arXiv (restricted to the arXiv categories the LLM named) and
     OpenAlex (restricted to open access and to the Computer Science
     field), collect the candidates, throw away the ones whose title or
     abstract contains one of the exclude terms,
  2. download the open-access PDFs into the project's corpus directory.

Contract with the adapter (crates/task-worker/src/paperqa.rs):
  argv[1]            path to a JSON file (see INPUT below)
  --fixture <dir>    tests only: read canned responses from <dir> instead
                     of the network (no HTTP request is made at all)
  stdout             one message per line: "progress: <text>" while
                     running, and exactly one final line
                     "TASKD_ACQUIRE {"candidates": n, "pdfs": m,
                                     "engines": {"arxiv": a, "openalex": b},
                                     "queries": q, "excluded": x,
                                     "query_source": "llm"|"fallback"}"
  exit               0 on success, non-zero on failure (short message on
                     stderr). The adapter keeps going either way: the
                     evidence gate (ADR-0035 D3) decides.

INPUT (all paths absolute):
  {"queries": ["asynchronous I/O runtime", ...],   # deterministic fallback
   "query_llm": {"enabled": true, "model": "qwen3.8-27b",
                 "base_url": "http://127.0.0.1:18000/v1" | null,
                 "api_key": "unused" | null,
                 "timeout_secs": 300, "max_tokens": 2000,
                 "max_queries": 6},                # absent/enabled=false: no LLM call
   "request": {"title": "...", "objective": "...", "context": "..."},
   "paper_directory": "<papers>/<project_id>",     # the project corpus
   "papers_path":     "<ws>/artifacts/papers.json",
   "sources_path":    "<ws>/artifacts/sources.json",
   "queries_path":    "<ws>/artifacts/queries.json",
   "arxiv_categories": ["cs.DC", "cs.OS", "cs.PF", "cs.NI"],   # default
   "openalex_filter": "is_oa:true,primary_topic.field.id:17",
   "max_candidates": 30, "max_pdfs": 12, "per_query": 20,
   "timeout_secs": 30, "mailto": "you@example.org" | null}

OUTPUT FILES
  queries.json     {"generated_by": "llm"|"fallback", "model": ...,
                    "queries": [{text, engines[], arxiv_categories[]}],
                    "exclude_terms": [...], "raw": "<what the LLM said>",
                    "error": "<why we fell back>"}
  papers.json      [{title, authors[], year, venue, doi, arxiv_id, url,
                     pdf_url, file, pdf_downloaded, source_engine,
                     query_text, abstract}]
  sources.json     [{url, title, engine, cited}]  -- same shape as the
                   LDR runner writes (ADR-0031 D1). `cited` is always
                   false here; the adapter fills it in after `pqa`
                   answers (ADR-0035 D2), because only then is there an
                   answer to match against.

Everything after step 0 is mechanical: the exclusion, the de-duplication
and the ordering of the candidates ("relevance", round-robin across the
(query, engine) result lists so that no single query crowds the list out)
involve no LLM at all.

Only the standard library is used, and every network call goes through
`Fetcher.get`/`Fetcher.post`, so the module can be imported on its own
(e.g. to unit test `normalize_title`, `arxiv_url` or `parse_query_plan`)
and can run end to end against a fixture directory without touching the
network.
"""

import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

USER_AGENT = "taskd-paperqa-acquire/1.0 (deterministic literature acquisition for taskd)"

ARXIV_ENDPOINT = "https://export.arxiv.org/api/query"
OPENALEX_ENDPOINT = "https://api.openalex.org/works"

ATOM_NS = "{http://www.w3.org/2005/Atom}"
ARXIV_NS = "{http://arxiv.org/schemas/atom}"

KNOWN_ENGINES = ("arxiv", "openalex")
# ADR-0035 D5: arXiv is asked for these categories when the LLM names none.
DEFAULT_ARXIV_CATEGORIES = ("cs.DC", "cs.OS", "cs.PF", "cs.NI")
# `primary_topic.field.id:17` = Computer Science (verified against
# `GET https://api.openalex.org/fields`, 2026-09-18: fields/17 = Computer Science).
DEFAULT_OPENALEX_FILTER = "is_oa:true,primary_topic.field.id:17"
# How many words of one query we are willing to AND together on arXiv.
MAX_QUERY_WORDS = 8
# Words that would only dilute an arXiv AND-query.
QUERY_STOPWORDS = frozenset(
    ("a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is",
     "of", "on", "or", "that", "the", "to", "via", "with")
)
ABSTRACT_MAX_CHARS = 600


# ---------------------------------------------------------------- fetching


class Fetcher:
    """`url -> bytes`. With `fixture_dir` set, no HTTP request is made at
    all: canned bodies are read from the directory (tests; ADR-0035 §4.1).

    Fixture lookup, in order:
      arXiv      <dir>/arxiv-<i>.xml    (i = 1, 2, ... call order)
                 <dir>/arxiv.xml
                 an empty Atom feed
      OpenAlex   <dir>/openalex-<i>.json
                 <dir>/openalex.json
                 {"results": []}
      LLM        <dir>/llm-<i>.json     (an OpenAI `chat/completions` body)
                 <dir>/llm.json
                 an empty answer (the runner then falls back)
      PDF        <dir>/pdf-<basename of the url>
                 b"%PDF-1.4\\nfixture\\n"
    """

    def __init__(self, timeout, fixture_dir=None):
        self.timeout = timeout
        self.fixture_dir = fixture_dir
        self.calls = {"arxiv": 0, "openalex": 0, "pdf": 0, "llm": 0}

    def _fixture(self, kind, url):
        self.calls[kind] = self.calls.get(kind, 0) + 1
        index = self.calls[kind]
        if kind == "pdf":
            name = os.path.basename(urllib.parse.urlparse(url).path) or "paper"
            path = os.path.join(self.fixture_dir, "pdf-" + name)
            if os.path.isfile(path):
                with open(path, "rb") as handle:
                    return handle.read()
            return b"%PDF-1.4\nfixture\n"
        suffix = "xml" if kind == "arxiv" else "json"
        for candidate in (
            os.path.join(self.fixture_dir, "%s-%d.%s" % (kind, index, suffix)),
            os.path.join(self.fixture_dir, "%s.%s" % (kind, suffix)),
        ):
            if os.path.isfile(candidate):
                with open(candidate, "rb") as handle:
                    return handle.read()
        if kind == "arxiv":
            return b'<?xml version="1.0" encoding="UTF-8"?><feed xmlns="http://www.w3.org/2005/Atom"></feed>'
        if kind == "llm":
            return b'{"choices": [{"message": {"content": ""}}]}'
        return b'{"results": []}'

    def get(self, kind, url):
        if self.fixture_dir:
            return self._fixture(kind, url)
        self.calls[kind] = self.calls.get(kind, 0) + 1
        request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
        with urllib.request.urlopen(request, timeout=self.timeout) as response:
            return response.read()

    def post(self, kind, url, payload, timeout=None, headers=None):
        """JSON POST. Fixtures answer without any HTTP request (tests)."""
        if self.fixture_dir:
            return self._fixture(kind, url)
        self.calls[kind] = self.calls.get(kind, 0) + 1
        body = json.dumps(payload).encode("utf-8")
        merged = {"User-Agent": USER_AGENT, "Content-Type": "application/json"}
        merged.update(headers or {})
        request = urllib.request.Request(url, data=body, headers=merged, method="POST")
        with urllib.request.urlopen(request, timeout=timeout or self.timeout) as response:
            return response.read()


# ------------------------------------------------- step 0: the LLM writes the queries


LLM_SYSTEM_PROMPT = (
    "You build search queries for academic literature search engines (arXiv and OpenAlex). "
    "You answer with one JSON object and nothing else."
)

LLM_USER_TEMPLATE = """Request title: {title}

Request:
{objective}
{context}
Write 3 to 6 search queries that will find recent academic papers for this request.

Rules:
- English only, even when the request is written in another language.
- Each query is a short phrase (3 to 6 words) of the kind that appears in the title or the
  abstract of a paper, not a sentence and not a list of keywords.
- Never use a project or product name on its own (for example "Pluvio"): outside this project
  nobody indexes that word. Either pair it with the words that say what it is, or drop it.
- An ambiguous word must carry its field. "ad hoc" on its own finds mobile ad hoc networks, so
  write "ad hoc file system HPC". "runtime" on its own finds programming language runtimes, so
  write "asynchronous I/O runtime storage".
- "engines" lists which of "arxiv" and "openalex" to search for that query (use both unless one
  of them is clearly wrong for it).
- "arxiv_categories" lists the arXiv categories the query should be restricted to, for example
  ["cs.DC","cs.OS","cs.PF"].
- "exclude_terms" lists 3 to 10 lower-case phrases that mark a paper as off topic for THIS
  request: a candidate whose title or abstract contains one of them is thrown away. Name the
  near-homonyms you expect the search engines to return by mistake.

Answer with exactly this shape and nothing else:
{{"queries": [{{"text": "...", "engines": ["arxiv", "openalex"], "arxiv_categories": ["cs.DC"]}}],
 "exclude_terms": ["..."]}}
"""


def build_query_messages(request):
    """The one prompt of the query-writing call (ADR-0035 D5)."""
    context = str((request or {}).get("context") or "").strip()
    context_block = "\nWhat this project is about:\n%s\n" % context if context else ""
    user = LLM_USER_TEMPLATE.format(
        title=str((request or {}).get("title") or "(no title)").strip(),
        objective=str((request or {}).get("objective") or "").strip(),
        context=context_block,
    )
    return [
        {"role": "system", "content": LLM_SYSTEM_PROMPT},
        {"role": "user", "content": user},
    ]


def chat_completions_url(base_url):
    base = str(base_url or "").strip().rstrip("/")
    if not base:
        return ""
    if base.endswith("/chat/completions"):
        return base
    return base + "/chat/completions"


def message_text(body):
    """The assistant's text out of an OpenAI-compatible response body.

    Reasoning models (the local Qwen) put their thinking in `reasoning` /
    `reasoning_content` and may leave `content` empty when they run out of
    tokens; the thinking is used as a last resort so that a JSON object in
    it is still found."""
    payload = json.loads(body)
    choices = payload.get("choices") or []
    if not choices or not isinstance(choices[0], dict):
        raise ValueError("no choices in the response")
    message = choices[0].get("message") or {}
    for key in ("content", "reasoning_content", "reasoning"):
        text = message.get(key)
        if isinstance(text, str) and text.strip():
            return text
    raise ValueError("the model returned an empty message")


def extract_json_object(text):
    """The first balanced `{...}` in `text`, with `<think>` blocks and
    markdown fences taken off first. `None` if there is none."""
    cleaned = re.sub(r"(?is)<think>.*?</think>", " ", str(text or ""))
    cleaned = re.sub(r"(?is)<think>.*$", " ", cleaned)
    cleaned = re.sub(r"```[a-zA-Z]*", " ", cleaned).replace("```", " ")
    start = cleaned.find("{")
    while start != -1:
        depth = 0
        in_string = False
        escaped = False
        for index in range(start, len(cleaned)):
            char = cleaned[index]
            if in_string:
                if escaped:
                    escaped = False
                elif char == "\\":
                    escaped = True
                elif char == '"':
                    in_string = False
                continue
            if char == '"':
                in_string = True
            elif char == "{":
                depth += 1
            elif char == "}":
                depth -= 1
                if depth == 0:
                    return cleaned[start : index + 1]
        start = cleaned.find("{", start + 1)
    return None


def clean_category(raw):
    """`cs.DC` / `math.NA` and nothing else (the value goes into a URL)."""
    text = str(raw or "").strip()
    return text if re.match(r"^[A-Za-z][A-Za-z-]*\.[A-Za-z][A-Za-z-]*$", text) else ""


def clean_query_text(raw):
    """One query phrase: collapse the whitespace, keep it short, and refuse
    anything that is not usable as a search term."""
    text = " ".join(str(raw or "").split())
    text = text.strip("\"'`.,;:")
    if not text:
        return ""
    words = [w for w in text.split(" ") if w]
    if len(words) > MAX_QUERY_WORDS:
        words = words[:MAX_QUERY_WORDS]
    text = " ".join(words)
    # A query the search engines can index: it must carry ASCII letters.
    if not re.search(r"[A-Za-z]{2}", text):
        return ""
    return text


def parse_query_plan(text, default_categories, max_queries):
    """The LLM's answer -> (queries, exclude_terms). Raises `ValueError` if
    the answer is not usable (the caller then falls back to the
    deterministic terms; ADR-0035 D5)."""
    blob = extract_json_object(text)
    if not blob:
        raise ValueError("no JSON object in the answer")
    try:
        parsed = json.loads(blob)
    except ValueError as exc:
        raise ValueError("the JSON object is broken: %s" % exc)
    if not isinstance(parsed, dict):
        raise ValueError("the JSON is not an object")

    raw_queries = parsed.get("queries")
    if not isinstance(raw_queries, list):
        raise ValueError("`queries` is not a list")

    queries = []
    seen = set()
    for item in raw_queries:
        if isinstance(item, str):
            item = {"text": item}
        if not isinstance(item, dict):
            continue
        text_value = clean_query_text(item.get("text") or item.get("query"))
        if not text_value or text_value.lower() in seen:
            continue
        engines = [e for e in (item.get("engines") or []) if str(e).strip().lower() in KNOWN_ENGINES]
        engines = list(dict.fromkeys(str(e).strip().lower() for e in engines)) or list(KNOWN_ENGINES)
        categories = [c for c in (clean_category(c) for c in (item.get("arxiv_categories") or [])) if c]
        if not categories:
            categories = list(default_categories)
        seen.add(text_value.lower())
        queries.append({"text": text_value, "engines": engines, "arxiv_categories": categories})
        if len(queries) >= max_queries:
            break
    if not queries:
        raise ValueError("no usable query in `queries`")

    exclude_terms = []
    for term in parsed.get("exclude_terms") or []:
        cleaned = " ".join(str(term or "").split()).lower()
        if len(cleaned) >= 3 and cleaned not in exclude_terms:
            exclude_terms.append(cleaned)
    return queries, exclude_terms


def fallback_queries(queries, default_categories):
    """The adapter's deterministic terms in the same shape (ADR-0035 D5:
    what we use when the LLM answer is unusable)."""
    out = []
    seen = set()
    for raw in queries or []:
        text = clean_query_text(raw)
        if not text or text.lower() in seen:
            continue
        seen.add(text.lower())
        out.append({"text": text, "engines": list(KNOWN_ENGINES), "arxiv_categories": list(default_categories)})
    return out


def plan_queries(payload, fetcher, progress):
    """Step 0 (ADR-0035 D5): ask the LLM for the search terms; fall back to
    the deterministic ones on any problem. Returns (queries, exclude_terms,
    plan) where `plan` is what goes into `artifacts/queries.json`."""
    default_categories = [c for c in (clean_category(c) for c in (payload.get("arxiv_categories") or [])) if c]
    if not default_categories:
        default_categories = list(DEFAULT_ARXIV_CATEGORIES)
    deterministic = fallback_queries(payload.get("queries"), default_categories)

    config = payload.get("query_llm") or {}
    plan = {
        "generated_by": "fallback",
        "model": str(config.get("model") or ""),
        "queries": deterministic,
        "exclude_terms": [],
        "raw": "",
        "error": "",
    }
    if not config.get("enabled"):
        plan["error"] = "the query LLM is off (acquire.query_llm = false)"
        progress("search terms: the deterministic extraction (the query LLM is off)")
        return deterministic, [], plan

    model = str(config.get("model") or "").strip()
    url = chat_completions_url(config.get("base_url") or os.environ.get("OPENAI_BASE_URL"))
    api_key = config.get("api_key") or os.environ.get("OPENAI_API_KEY") or "unused"
    if not model or not url:
        plan["error"] = "no model or no OPENAI_BASE_URL for the query LLM"
        progress("search terms: falling back to the deterministic extraction (%s)" % plan["error"])
        return deterministic, [], plan

    body = {
        "model": model,
        "messages": build_query_messages(payload.get("request")),
        "temperature": 0.0,
        "max_tokens": int(config.get("max_tokens") or 2000),
        "stream": False,
        # vLLM / Qwen3: without this the model spends the whole budget on its
        # thinking and `content` comes back empty (verified 2026-09-18).
        "chat_template_kwargs": {"enable_thinking": False},
    }
    headers = {"Authorization": "Bearer %s" % api_key}
    timeout = float(config.get("timeout_secs") or 300)
    max_queries = int(config.get("max_queries") or 6)

    progress("asking %s for the search terms" % model)
    text = ""
    error = ""
    for attempt in (1, 2):
        try:
            raw = fetcher.post("llm", url, body, timeout=timeout, headers=headers)
            text = message_text(raw)
            error = ""
            break
        except urllib.error.HTTPError as exc:
            error = "HTTP %s from %s" % (exc.code, url)
            # Not every OpenAI-compatible server knows `chat_template_kwargs`.
            if attempt == 1 and "chat_template_kwargs" in body:
                del body["chat_template_kwargs"]
                progress("the endpoint rejected chat_template_kwargs; retrying without it")
                continue
            break
        except Exception as exc:
            error = "%s: %s" % (type(exc).__name__, exc)
            break

    plan["raw"] = text[:4000]
    if error or not text:
        plan["error"] = error or "the model returned nothing"
        progress("search terms: falling back to the deterministic extraction (%s)" % plan["error"])
        return deterministic, [], plan

    try:
        queries, exclude_terms = parse_query_plan(text, default_categories, max_queries)
    except ValueError as exc:
        plan["error"] = str(exc)
        progress("search terms: falling back to the deterministic extraction (%s)" % exc)
        return deterministic, [], plan

    plan["generated_by"] = "llm"
    plan["queries"] = queries
    plan["exclude_terms"] = exclude_terms
    for query in queries:
        progress("search term: %r (%s; %s)" % (query["text"], ", ".join(query["engines"]), ", ".join(query["arxiv_categories"])))
    if exclude_terms:
        progress("exclude terms: %s" % "; ".join(exclude_terms))
    return queries, exclude_terms, plan


# ------------------------------------------------------------ search / parse


def arxiv_url(query, per_query, categories=None, join="AND"):
    """arXiv query URL (ADR-0035 D5).

    The words are **not** quoted (the real API returns `totalResults 0` for
    `all:"ad-hoc file system"`) and they are joined with `AND`: `all:a b c`
    is read by the API as `all:a OR all:b OR all:c`, which is what dragged
    "mobile ad hoc networks" into the 2026-09-18 production run. The
    categories become `AND (cat:cs.DC OR cat:cs.OS)`, which is what keeps
    the networking papers out (verified against the real API, 2026-09-18)."""
    words = [w for w in re.split(r"\s+", str(query or "").strip()) if w]
    kept = [w for w in words if w.lower() not in QUERY_STOPWORDS] or words
    operator = " OR " if str(join).upper() == "OR" else " AND "
    search = operator.join("all:" + w for w in kept)
    if len(kept) > 1 and operator == " OR ":
        search = "(" + search + ")"
    cats = [c for c in (clean_category(c) for c in (categories or [])) if c]
    if cats:
        search += " AND (" + " OR ".join("cat:" + c for c in cats) + ")"
    params = {
        "search_query": search,
        "start": "0",
        "max_results": str(per_query),
        "sortBy": "relevance",
        "sortOrder": "descending",
    }
    return ARXIV_ENDPOINT + "?" + urllib.parse.urlencode(params)


def openalex_url(query, per_query, mailto=None, filter_expr=None):
    """OpenAlex `/works` URL. The filter is open access **and** the field
    (Computer Science by default; ADR-0035 D5)."""
    params = {
        "search": str(query or "").strip(),
        "per_page": str(per_query),
        "filter": str(filter_expr or DEFAULT_OPENALEX_FILTER),
    }
    if mailto:
        params["mailto"] = mailto
    return OPENALEX_ENDPOINT + "?" + urllib.parse.urlencode(params)


def _text(element):
    return " ".join((element.text or "").split()) if element is not None else ""


def arxiv_id_from(raw):
    """`http://arxiv.org/abs/1003.3565v1` -> `1003.3565v1`."""
    if not raw:
        return ""
    return raw.rstrip("/").split("/abs/")[-1] if "/abs/" in raw else raw.rstrip("/").split("/")[-1]


def parse_arxiv(body):
    """arXiv's Atom feed -> candidate dicts (in the order the feed lists
    them, which is the relevance order we asked for)."""
    try:
        root = ET.fromstring(body)
    except ET.ParseError:
        return []
    out = []
    for entry in root.findall(ATOM_NS + "entry"):
        raw_id = _text(entry.find(ATOM_NS + "id"))
        arxiv_id = arxiv_id_from(raw_id)
        if not arxiv_id:
            continue
        title = _text(entry.find(ATOM_NS + "title"))
        published = _text(entry.find(ATOM_NS + "published"))
        year = None
        if len(published) >= 4 and published[:4].isdigit():
            year = int(published[:4])
        authors = []
        for author in entry.findall(ATOM_NS + "author"):
            name = _text(author.find(ATOM_NS + "name"))
            if name:
                authors.append(name)
        doi = _text(entry.find(ARXIV_NS + "doi"))
        venue = _text(entry.find(ARXIV_NS + "journal_ref")) or "arXiv"
        pdf_url = ""
        for link in entry.findall(ATOM_NS + "link"):
            if link.get("title") == "pdf" or link.get("type") == "application/pdf":
                pdf_url = link.get("href") or ""
        if not pdf_url:
            pdf_url = "https://arxiv.org/pdf/" + arxiv_id
        out.append(
            {
                "title": title,
                "authors": authors,
                "year": year,
                "venue": venue,
                "doi": doi,
                "arxiv_id": arxiv_id,
                "url": raw_id.replace("http://", "https://") or pdf_url,
                "pdf_url": pdf_url,
                "abstract": _text(entry.find(ATOM_NS + "summary"))[:ABSTRACT_MAX_CHARS],
                "source_engine": "arxiv",
            }
        )
    return out


def openalex_abstract(work):
    """OpenAlex hands the abstract over as an inverted index; put it back
    together (it is what the exclude terms are matched against)."""
    inverted = work.get("abstract_inverted_index")
    if not isinstance(inverted, dict):
        return ""
    positions = []
    for word, places in inverted.items():
        if not isinstance(places, list):
            continue
        for place in places:
            if isinstance(place, int):
                positions.append((place, str(word)))
    positions.sort()
    return " ".join(word for _, word in positions)[:ABSTRACT_MAX_CHARS]


def parse_openalex(body):
    """OpenAlex `/works` -> candidate dicts (relevance order as returned)."""
    try:
        payload = json.loads(body)
    except (ValueError, TypeError):
        return []
    out = []
    for work in payload.get("results") or []:
        if not isinstance(work, dict):
            continue
        title = " ".join(str(work.get("title") or work.get("display_name") or "").split())
        if not title:
            continue
        doi = str(work.get("doi") or "")
        primary = work.get("primary_location") or {}
        best_oa = work.get("best_oa_location") or {}
        open_access = work.get("open_access") or {}
        source = (primary.get("source") or {}) if isinstance(primary, dict) else {}
        pdf_url = ""
        for candidate in (
            best_oa.get("pdf_url") if isinstance(best_oa, dict) else None,
            primary.get("pdf_url") if isinstance(primary, dict) else None,
            open_access.get("oa_url") if isinstance(open_access, dict) else None,
        ):
            if candidate:
                pdf_url = str(candidate)
                break
        authors = []
        for authorship in work.get("authorships") or []:
            if not isinstance(authorship, dict):
                continue
            name = ((authorship.get("author") or {}) or {}).get("display_name")
            if name:
                authors.append(str(name))
        out.append(
            {
                "title": title,
                "authors": authors,
                "year": work.get("publication_year"),
                "venue": str((source or {}).get("display_name") or ""),
                "doi": doi,
                "arxiv_id": "",
                "url": doi or str(work.get("id") or ""),
                "pdf_url": pdf_url,
                "abstract": openalex_abstract(work),
                "source_engine": "openalex",
            }
        )
    return out


# --------------------------------------------------------------- exclusion


def candidate_excluded(candidate, exclude_terms):
    """The exclude term that throws this candidate away, or `""`
    (ADR-0035 D5 step 3: title + abstract, decided mechanically)."""
    if not exclude_terms:
        return ""
    haystack = ("%s %s" % (candidate.get("title") or "", candidate.get("abstract") or "")).lower()
    for term in exclude_terms:
        if term and term in haystack:
            return term
    return ""


def drop_excluded(candidates, exclude_terms, progress):
    """Returns (kept, number dropped)."""
    kept = []
    dropped = 0
    for candidate in candidates:
        term = candidate_excluded(candidate, exclude_terms)
        if term:
            dropped += 1
            progress("excluded (%r): %s" % (term, (candidate.get("title") or "")[:90]))
            continue
        kept.append(candidate)
    return kept, dropped


# ------------------------------------------------------------- de-duplication


def normalize_doi(doi):
    """`https://doi.org/10.1/X` / `doi:10.1/x` / `10.1/X` -> `10.1/x`."""
    text = str(doi or "").strip().lower()
    for prefix in ("https://doi.org/", "http://doi.org/", "doi:"):
        if text.startswith(prefix):
            text = text[len(prefix) :]
    return text.strip("/")


def normalize_arxiv_id(arxiv_id):
    """Drop the version suffix so `1003.3565v1` and `1003.3565v2` are one paper."""
    text = str(arxiv_id or "").strip().lower()
    return re.sub(r"v\d+$", "", text)


def normalize_title(title):
    """Lower-case, letters and digits only. Used as the last-resort identity."""
    return re.sub(r"[^a-z0-9]+", "", str(title or "").lower())


def candidate_keys(candidate):
    keys = []
    doi = normalize_doi(candidate.get("doi"))
    if doi:
        keys.append("doi:" + doi)
    arxiv_id = normalize_arxiv_id(candidate.get("arxiv_id"))
    if arxiv_id:
        keys.append("arxiv:" + arxiv_id)
    title = normalize_title(candidate.get("title"))
    if title:
        keys.append("title:" + title)
    return keys


def interleave(lists):
    """Round-robin over the result lists so the relevance order of every
    (query, engine) pair gets a turn (ADR-0035 D1 step 2)."""
    out = []
    depth = max((len(items) for items in lists), default=0)
    for rank in range(depth):
        for items in lists:
            if rank < len(items):
                out.append(items[rank])
    return out


def dedupe_candidates(candidates, limit):
    """De-duplicate by DOI / arXiv id / normalized title, keeping the first
    occurrence, and cut the list at `limit`. Fields the first copy is
    missing (doi, pdf_url, ...) are filled in from the later duplicate."""
    seen = {}
    out = []
    for candidate in candidates:
        keys = candidate_keys(candidate)
        hit = None
        for key in keys:
            if key in seen:
                hit = seen[key]
                break
        if hit is not None:
            kept = out[hit]
            for field in ("doi", "arxiv_id", "pdf_url", "venue", "url", "abstract"):
                if not kept.get(field) and candidate.get(field):
                    kept[field] = candidate[field]
            if not kept.get("year") and candidate.get("year"):
                kept["year"] = candidate["year"]
            if not kept.get("authors") and candidate.get("authors"):
                kept["authors"] = candidate["authors"]
            for key in keys:
                seen.setdefault(key, hit)
            continue
        if limit is not None and len(out) >= limit:
            continue
        index = len(out)
        for key in keys:
            seen.setdefault(key, index)
        out.append(dict(candidate))
    return out


# ------------------------------------------------------------------ download


def slugify(text, max_len=48):
    slug = re.sub(r"[^a-z0-9]+", "-", str(text or "").lower()).strip("-")
    return slug[:max_len].strip("-")


def first_surname(authors):
    """`"André Brinkmann"` -> `brinkmann`. Empty string if unknown."""
    if not authors:
        return ""
    parts = [p for p in re.split(r"\s+", str(authors[0]).strip()) if p]
    if not parts:
        return ""
    return slugify(parts[-1], 24)


def pdf_filename(candidate):
    """Deterministic file name inside the corpus. The name is also what
    PaperQA2 keys its citations on when `parsing.use_doc_details = false`
    (ADR-0027's settings), so it carries the author and the year."""
    surname = first_surname(candidate.get("authors")) or "anon"
    year = candidate.get("year")
    year_part = str(year) if year else "nd"
    if candidate.get("arxiv_id"):
        tail = slugify("arxiv-" + str(candidate["arxiv_id"]), 32)
    elif candidate.get("doi"):
        tail = slugify(normalize_doi(candidate["doi"]), 32)
    else:
        tail = slugify(candidate.get("title"), 32)
    return "%s%s_%s.pdf" % (surname, year_part, tail or "paper")


def download_pdfs(candidates, paper_directory, max_pdfs, fetcher, progress):
    """Download at most `max_pdfs` open-access PDFs into `paper_directory`.
    Files that are already there are counted but **not** fetched again
    (ADR-0035 D1 step 3). Returns the number of PDFs in the corpus for
    this run's candidates."""
    os.makedirs(paper_directory, exist_ok=True)
    have = 0
    for candidate in candidates:
        if have >= max_pdfs:
            break
        name = pdf_filename(candidate)
        candidate["file"] = name
        path = os.path.join(paper_directory, name)
        if os.path.isfile(path) and os.path.getsize(path) > 0:
            candidate["pdf_downloaded"] = True
            have += 1
            progress("already in the corpus: %s" % name)
            continue
        url = candidate.get("pdf_url")
        if not url:
            continue
        try:
            body = fetcher.get("pdf", url)
        except Exception as exc:  # network / HTTP errors are per-paper, not fatal
            progress("could not download %s: %s" % (url, exc))
            continue
        if body[:4] != b"%PDF":
            progress("not a PDF, skipped: %s" % url)
            continue
        try:
            with open(path, "wb") as handle:
                handle.write(body)
        except OSError as exc:
            progress("could not write %s: %s" % (path, exc))
            continue
        candidate["pdf_downloaded"] = True
        have += 1
        progress("downloaded %s (%d bytes)" % (name, len(body)))
    return have


# ---------------------------------------------------------------------- main


def make_progress_printer():
    def progress(text):
        collapsed = " ".join(str(text).split())
        if collapsed:
            print("progress: %s" % collapsed, flush=True)

    return progress


def search_one(kind, query, per_query, mailto, openalex_filter, fetcher, progress):
    """One (query, engine) search. arXiv is asked with the words AND-ed
    first; if that is empty the same words are asked OR-ed (still inside
    the categories), because a long AND-query often has no hit at all
    (verified against the real API, 2026-09-18)."""
    text = query["text"]
    if kind == "openalex":
        url = openalex_url(text, per_query, mailto, openalex_filter)
        body = fetcher.get(kind, url)
        found = parse_openalex(body)
        progress("openalex: %d result(s) for %r" % (len(found), text))
        return found

    url = arxiv_url(text, per_query, query.get("arxiv_categories"), "AND")
    found = parse_arxiv(fetcher.get(kind, url))
    progress("arxiv: %d result(s) for %r" % (len(found), text))
    if not found and len(text.split(" ")) > 1:
        url = arxiv_url(text, per_query, query.get("arxiv_categories"), "OR")
        found = parse_arxiv(fetcher.get(kind, url))
        progress("arxiv (any word): %d result(s) for %r" % (len(found), text))
    return found


def search_all(queries, per_query, mailto, openalex_filter, fetcher, progress):
    """Run every (query, engine) search and return the result lists in the
    order they were issued (arXiv first for each query). Every candidate
    carries the query that found it (`query_text`; ADR-0035 D5 step 4)."""
    lists = []
    for query in queries:
        for kind in KNOWN_ENGINES:
            if kind not in query.get("engines", KNOWN_ENGINES):
                continue
            try:
                found = search_one(kind, query, per_query, mailto, openalex_filter, fetcher, progress)
            except Exception as exc:
                progress("%s search failed for %r: %s" % (kind, query["text"], exc))
                lists.append([])
                continue
            for candidate in found:
                candidate["query_text"] = query["text"]
            lists.append(found)
    return lists


def acquire(payload, fetcher, progress):
    per_query = int(payload.get("per_query") or 20)
    max_candidates = int(payload.get("max_candidates") or 0)
    max_pdfs = int(payload.get("max_pdfs") or 0)
    mailto = payload.get("mailto") or None
    openalex_filter = payload.get("openalex_filter") or DEFAULT_OPENALEX_FILTER
    paper_directory = payload["paper_directory"]

    queries, exclude_terms, plan = plan_queries(payload, fetcher, progress)

    lists = search_all(queries, per_query, mailto, openalex_filter, fetcher, progress)
    kept = []
    excluded = 0
    for found in lists:
        survivors, dropped = drop_excluded(found, exclude_terms, progress)
        excluded += dropped
        kept.append(survivors)
    if excluded:
        progress("%d result(s) dropped by the exclude terms" % excluded)

    candidates = dedupe_candidates(interleave(kept), max_candidates)
    for candidate in candidates:
        candidate.setdefault("file", "")
        candidate.setdefault("pdf_downloaded", False)
        candidate.setdefault("query_text", "")
        candidate.setdefault("abstract", "")
    progress("%d candidate paper(s) after de-duplication" % len(candidates))

    pdfs = download_pdfs(candidates, paper_directory, max_pdfs, fetcher, progress)

    engines = {"arxiv": 0, "openalex": 0}
    for candidate in candidates:
        engine = candidate.get("source_engine") or "unknown"
        engines[engine] = engines.get(engine, 0) + 1

    sources = [
        {
            "url": candidate.get("url") or candidate.get("pdf_url") or "",
            "title": candidate.get("title") or "",
            "engine": candidate.get("source_engine"),
            # ADR-0035 D2: the adapter fills this in once `pqa` has answered.
            "cited": False,
        }
        for candidate in candidates
    ]
    counts = {
        "candidates": len(candidates),
        "pdfs": pdfs,
        "engines": engines,
        "queries": len(queries),
        "excluded": excluded,
        "query_source": plan["generated_by"],
    }
    return candidates, sources, plan, counts


def write_json(path, value):
    directory = os.path.dirname(path) or "."
    os.makedirs(directory, exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(value, handle, indent=2, ensure_ascii=False)
        handle.write("\n")


def main():
    argv = sys.argv[1:]
    fixture_dir = None
    if "--fixture" in argv:
        index = argv.index("--fixture")
        if index + 1 >= len(argv):
            print("--fixture needs a directory", file=sys.stderr)
            return 2
        fixture_dir = argv[index + 1]
        del argv[index : index + 2]
    if not argv:
        print("usage: paperqa_acquire.py <input.json> [--fixture <dir>]", file=sys.stderr)
        return 2

    with open(argv[0], "r", encoding="utf-8") as handle:
        payload = json.load(handle)

    progress = make_progress_printer()
    fetcher = Fetcher(float(payload.get("timeout_secs") or 30), fixture_dir)
    try:
        candidates, sources, plan, counts = acquire(payload, fetcher, progress)
    except Exception as exc:
        print("literature acquisition failed: %s" % exc, file=sys.stderr)
        return 1

    write_json(payload["papers_path"], candidates)
    write_json(payload["sources_path"], sources)
    if payload.get("queries_path"):
        write_json(payload["queries_path"], plan)
    print("TASKD_ACQUIRE " + json.dumps(counts))
    return 0


if __name__ == "__main__":
    sys.exit(main())
