#!/usr/bin/env python3
"""Runner embedded in the taskd `paperqa` adapter (ADR-0035 D1).

Before PaperQA2 can answer anything it needs papers. This runner is the
"go and find the papers" stage: it searches arXiv and OpenAlex with the
search terms the adapter extracted (deterministically, no LLM), collects
the candidates, and downloads the open-access PDFs into the project's
corpus directory.

Contract with the adapter (crates/task-worker/src/paperqa.rs):
  argv[1]            path to a JSON file (see INPUT below)
  --fixture <dir>    tests only: read canned responses from <dir> instead
                     of the network (no HTTP request is made at all)
  stdout             one message per line: "progress: <text>" while
                     running, and exactly one final line
                     "TASKD_ACQUIRE {"candidates": n, "pdfs": m,
                                     "engines": {"arxiv": a, "openalex": b}}"
  exit               0 on success, non-zero on failure (short message on
                     stderr). The adapter keeps going either way: the
                     evidence gate (ADR-0035 D3) decides.

INPUT (all paths absolute):
  {"queries": ["asynchronous I/O runtime", ...],   # from the adapter
   "paper_directory": "<papers>/<project_id>",     # the project corpus
   "candidates_path": "<ws>/artifacts/candidates.json",
   "sources_path":    "<ws>/artifacts/sources.json",
   "max_candidates": 30, "max_pdfs": 12, "per_query": 20,
   "timeout_secs": 30, "mailto": "you@example.org" | null}

OUTPUT FILES
  candidates.json  [{title, authors[], year, venue, doi, arxiv_id, url,
                     pdf_url, file, pdf_downloaded, source_engine}]
  sources.json     [{url, title, engine, cited}]  -- same shape as the
                   LDR runner writes (ADR-0031 D1). `cited` is always
                   false here; the adapter fills it in after `pqa`
                   answers (ADR-0035 D2), because only then is there an
                   answer to match against.

Everything here is mechanical: no LLM is involved (ADR-0035 D1), and the
order of the candidates is "relevance", taken round-robin across the
(query, engine) result lists so that no single query crowds the list out.

Only the standard library is used, and every network call goes through
`Fetcher.get`, so the module can be imported on its own (e.g. to unit test
`normalize_title` or `dedupe_candidates`) and can run end to end against a
fixture directory without touching the network.
"""

import json
import os
import re
import sys
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

USER_AGENT = "taskd-paperqa-acquire/1.0 (deterministic literature acquisition for taskd)"

ARXIV_ENDPOINT = "https://export.arxiv.org/api/query"
OPENALEX_ENDPOINT = "https://api.openalex.org/works"

ATOM_NS = "{http://www.w3.org/2005/Atom}"
ARXIV_NS = "{http://arxiv.org/schemas/atom}"


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
      PDF        <dir>/pdf-<basename of the url>
                 b"%PDF-1.4\\nfixture\\n"
    """

    def __init__(self, timeout, fixture_dir=None):
        self.timeout = timeout
        self.fixture_dir = fixture_dir
        self.calls = {"arxiv": 0, "openalex": 0, "pdf": 0}

    def _fixture(self, kind, url):
        self.calls[kind] += 1
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
        return b'{"results": []}'

    def get(self, kind, url):
        if self.fixture_dir:
            return self._fixture(kind, url)
        self.calls[kind] += 1
        request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
        with urllib.request.urlopen(request, timeout=self.timeout) as response:
            return response.read()


# ------------------------------------------------------------ search / parse


def arxiv_url(query, per_query):
    """arXiv query URL. The phrase is **not** quoted: the real API returns
    `totalResults 0` for `all:"ad-hoc file system"` but 3 results for the
    same words unquoted (ADR-0035, verified 2026-09-18)."""
    words = [w for w in re.split(r"\s+", query.strip()) if w]
    search = "all:" + " ".join(words)
    params = {
        "search_query": search,
        "start": "0",
        "max_results": str(per_query),
        "sortBy": "relevance",
        "sortOrder": "descending",
    }
    return ARXIV_ENDPOINT + "?" + urllib.parse.urlencode(params)


def openalex_url(query, per_query, mailto=None):
    params = {"search": query.strip(), "per_page": str(per_query), "filter": "is_oa:true"}
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
                "source_engine": "arxiv",
            }
        )
    return out


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
                "source_engine": "openalex",
            }
        )
    return out


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
            for field in ("doi", "arxiv_id", "pdf_url", "venue", "url"):
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


def search_all(queries, per_query, mailto, fetcher, progress):
    """Run every (query, engine) search and return the result lists in the
    order they were issued (arXiv first for each query)."""
    lists = []
    for query in queries:
        for kind, url in (
            ("arxiv", arxiv_url(query, per_query)),
            ("openalex", openalex_url(query, per_query, mailto)),
        ):
            try:
                body = fetcher.get(kind, url)
            except Exception as exc:
                progress("%s search failed for %r: %s" % (kind, query, exc))
                lists.append([])
                continue
            found = parse_arxiv(body) if kind == "arxiv" else parse_openalex(body)
            progress("%s: %d result(s) for %r" % (kind, len(found), query))
            lists.append(found)
    return lists


def acquire(payload, fetcher, progress):
    queries = [q for q in (payload.get("queries") or []) if str(q).strip()]
    per_query = int(payload.get("per_query") or 20)
    max_candidates = int(payload.get("max_candidates") or 0)
    max_pdfs = int(payload.get("max_pdfs") or 0)
    mailto = payload.get("mailto") or None
    paper_directory = payload["paper_directory"]

    lists = search_all(queries, per_query, mailto, fetcher, progress)
    candidates = dedupe_candidates(interleave(lists), max_candidates)
    for candidate in candidates:
        candidate.setdefault("file", "")
        candidate.setdefault("pdf_downloaded", False)
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
    return candidates, sources, {"candidates": len(candidates), "pdfs": pdfs, "engines": engines}


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
        candidates, sources, counts = acquire(payload, fetcher, progress)
    except Exception as exc:
        print("literature acquisition failed: %s" % exc, file=sys.stderr)
        return 1

    write_json(payload["candidates_path"], candidates)
    write_json(payload["sources_path"], sources)
    print("TASKD_ACQUIRE " + json.dumps(counts))
    return 0


if __name__ == "__main__":
    sys.exit(main())
