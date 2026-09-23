#!/usr/bin/env python3
"""Runner embedded in the celeris `paperqa` adapter (ADR-0063 Phase 109d C1).

Replaces the `pqa ask` CLI (Phase 108/109/109b/109c) with the PaperQA2
**Python API** (`paperqa.ask`/`paperqa.Settings`), confirmed against the
production venv (`paperqa==2026.8.12`) by the parent agent (2026-09-23):

  paperqa.agents.ask(query: str, settings: Settings) -> AnswerResponse
  AnswerResponse.session -> PQASession(id, question, answer, raw_answer,
      answer_reasoning, has_successful_answer, context, contexts,
      references, formatted_answer, graded_answer, cost, token_counts,
      config_md5, tool_history)
  PQASession.contexts -> list[Context(id, context, question, text, score)]
  Context.text -> Text(embedding, text, name, media, doc)
  Text.doc -> Doc(embedding, docname, dockey, citation, content_hash, ...)
  Settings.from_name(name) reads `pqa_directory("settings")/<name>.json`, an
      **unconfigurable** `~/.pqa/settings/` path -- `PQA_SETTINGS_DIR` has no
      effect on it (confirmed by reading the production venv's source,
      paperqa==2026.8.12: `pqa_directory` never looks at that env var).
      ADR-0063 Phase 109e: this runner reads `settings_path` (a full path
      the adapter builds from `[adapters.paperqa] settings`) directly
      instead, redoing the same "validate then rebuild" two steps
      `from_name` itself does, and only falls back to `from_name(name)` when
      `settings_path` does not exist (still the only way to reach paperqa's
      own bundled config names, e.g. `"high_quality"`).

Why the switch (ADR-0063 Phase 109b A2 / Phase 109c P-109c-1): the CLI has no
`--output json`, so the adapter had to scrape the `References`/`Sources`
section out of the CLI's rich-formatted console text. That is a lossy proxy
for the evidence PaperQA2 actually used. The Python API hands back
`PQASession.contexts` directly -- the adapter now counts a candidate as
`cited` if any context's `docname`/`dockey` matches it (a citation from the
evidence the model actually retrieved), OR-ed with the old text-match against
the answer body (`answer_cites`, unchanged, still counts as auxiliary).

Contract with the adapter (crates/task-worker/src/paperqa.rs):
  argv[1]  path to a JSON file (see INPUT below)
  stdout   "progress: <text>" lines while running (no marker line -- the
           adapter reads the result straight from `output_path` once this
           process exits)
  exit     0 on success (an `output_path` JSON is always written, even if
           individual questions failed -- see `error` per answer),
           2 bad usage, or settings could not be resolved (ADR-0063 Phase
           109e: neither `settings_path` nor `Settings.from_name` found
           anything -- retrying will not help, the adapter reports this as
           `retryable: false`), **3 if `paperqa` cannot be imported** (the
           adapter turns this into a clear "tool not set up" error rather
           than a generic retryable failure), 1 any other failure before any
           question could be attempted.

INPUT (all paths absolute):
  {"settings_name": "celeris-proxy" | null,     # `Settings.from_name(...)` fallback only
   "settings_dir": "<.../settings>" | null,      # only used to rebuild `settings_path` if absent
   "settings_path": "<.../settings/celeris-proxy.json>" | null,  # read directly if it exists (ADR-0063 Phase 109e)
   "paper_directory": "<papers>/<project_id>",
   "index_directory": "<index>/<project_id>",
   "index_name": "<project_id>" | null,
   "model": "qwen3.8-27b" | null,                # optional `settings.llm` override
   "targets": ["CHFS", "FINCHFS", ...],          # ADR-0063 Phase 109c A
   "aspects": ["server/client 配置", ...],
   "comparison_target": "BenchFS" | null,
   "max_asks": 8,
   "fallback_question": "<the full single-question text used when targets is empty>",
   "output_path": "<run_dir>/ask_output.json"}

`questions` (the `[{id, target, question}]` list `ask()` is actually called
with, one per target plus one optional comparison summary -- ADR-0063 Phase
109d C3) is *derived* from the fields above by `build_questions_for_targets`,
not supplied by the adapter: that keeps the one prompt template that
production observation (Phase 109c) showed actually gets a structured answer
in a single place, testable on its own (`python3 -c`, no `paperqa` install
needed) rather than duplicated on the Rust side.

OUTPUT (`output_path`):
  {"answers": [{"id": "t1", "target": "CHFS", "question": "...",
                "answer": "...", "has_successful_answer": true,
                "contexts": [{"docname": "...", "dockey": "...",
                               "citation": "...", "score": 5,
                               "question": "t1"}, ...],
                "references": "...", "cost": 0.01, "token_counts": {...}},
               ...],
   "target_aspect_table": "| 対象 | ... |\n| --- | ... |\n..." }
  (or, if `paperqa` is not importable: {"error": "...", "answers": []})

Retries (ADR-0063 Phase 109d C1): each `ask()` call is retried up to 3 times
(2s / 4s / 8s backoff) on a transient failure (503 / 502 / 429 / connection
reset-refused-aborted / timeout -- the same llm-proxy failure modes ADR-0063
Phase 109b already retries for LDR and the litellm `num_retries` setting
already retries for PaperQA2's own internal calls). A question that still
fails is recorded with `"error"` set and an empty answer/contexts -- it does
not abort the other questions or the run.

Only the standard library is used outside of the guarded `import paperqa`,
so `build_questions_for_targets`/`build_target_aspect_table`/
`flatten_contexts` (and the retry helpers) can be exercised with
`python3 -c` against plain dicts, without the `paperqa` package installed
(ADR-0063 Phase 109d test requirement).
"""

import json
import os
import re
import sys
import time

DEFAULT_MAX_ASKS = 8


# --------------------------------------------------------- dict/attr duality
#
# Real `Context`/`Text`/`Doc`/`PQASession` objects are attribute-based
# (pydantic-style) dataclasses; tests that exercise `flatten_contexts` /
# `build_target_aspect_table` without installing `paperqa` pass plain nested
# dicts of the same shape instead. `_get` reads either.


def _get(obj, name, default=None):
    if obj is None:
        return default
    if isinstance(obj, dict):
        return obj.get(name, default)
    return getattr(obj, name, default)


# ------------------------------------------------------------- pure: contexts


def flatten_contexts(contexts, question_id):
    """Flatten one `ask()` answer's `PQASession.contexts` (or, in a test, a
    plain list of dicts of the same shape) into simple JSON-safe dicts
    (ADR-0063 Phase 109d C1/C4). `question_id` is stamped onto every entry so
    the adapter can report, per cited document, which question(s) used it."""
    flat = []
    for ctx in contexts or []:
        text = _get(ctx, "text")
        doc = _get(text, "doc")
        docname = _get(doc, "docname") or ""
        dockey = _get(doc, "dockey") or ""
        citation = _get(doc, "citation") or ""
        flat.append(
            {
                "docname": str(docname),
                "dockey": str(dockey),
                "citation": str(citation),
                "score": _get(ctx, "score"),
                "question": question_id,
            }
        )
    return flat


# --------------------------------------------------------------- pure: table


def _strip_echoed_question(answer_text, question_text=None):
    """Drop a leading echo of the question PaperQA (or the underlying model)
    was asked, so `_extract_aspect_line`'s substring fallback does not pick
    the question's own aspect list as every cell's answer (ADR-0063 Phase
    109f, observed run `01M37AZ129EMB93N50MZ132S8K`: every cell in
    `target_aspect_table` held the literal question text, because the
    question line -- built by `build_questions_for_targets` -- contains all
    the aspect names joined by `、`, and it was the first line of
    `answer_text`). Removes any line that starts with `Question:`/`質問:`/
    `質問：` (case-insensitive for the ASCII form), and any line equal
    (after stripping) to `question_text` when one is given."""
    lines = (answer_text or "").splitlines()
    question_text = (question_text or "").strip()
    kept = []
    for raw_line in lines:
        stripped = raw_line.strip()
        if not stripped:
            kept.append(raw_line)
            continue
        if stripped.lower().startswith("question:") or stripped.startswith(
            ("質問:", "質問：")
        ):
            continue
        if question_text and stripped == question_text:
            continue
        kept.append(raw_line)
    return "\n".join(kept)


def _extract_aspect_line(answer_text, aspect, question_text=None):
    """The one line of `answer_text` that answers `aspect` (the per-target
    question asks for `- <aspect>: <fact (citation)>` lines -- ADR-0063
    Phase 109d C3), or `"未確認"` if none is found. Strips an echoed
    question first (Phase 109f, see `_strip_echoed_question`). Tries an
    exact line-prefix match first (`- aspect: ...` / `**aspect**: ...`),
    then falls back to any line merely containing the aspect text."""
    cleaned = _strip_echoed_question(answer_text, question_text)
    lines = cleaned.splitlines()
    for raw_line in lines:
        stripped = raw_line.strip()
        if not stripped:
            continue
        plain = stripped.lstrip("-*").strip().replace("**", "")
        if plain.startswith(aspect):
            rest = plain[len(aspect) :].strip()
            rest = rest.lstrip(":：").strip()
            return rest or "未確認"
    for raw_line in lines:
        if aspect and aspect in raw_line:
            return raw_line.strip()
    return "未確認"


def build_target_aspect_table(targets, aspects, answers):
    """The target x aspect Markdown table for `answer.md`/`report.md`
    (ADR-0063 Phase 109d C4). Empty (`""`) if there are no targets or no
    aspects -- the caller falls back to the per-target sections alone. A
    cell whose aspect does not show up in that target's answer is
    `"未確認"` (never guessed). Phase 109f: also strips a leading echo of
    the question (`answer.question`, when the answer dict/object has one)
    out of the answer text before looking for aspect lines."""
    targets = [str(t).strip() for t in (targets or []) if str(t or "").strip()]
    aspects = [str(a).strip() for a in (aspects or []) if str(a or "").strip()]
    if not targets or not aspects:
        return ""
    by_target = {}
    question_by_target = {}
    for answer in answers or []:
        target = _get(answer, "target")
        if target:
            by_target[str(target)] = _get(answer, "answer") or ""
            question_by_target[str(target)] = _get(answer, "question") or ""
    lines = [
        "| 対象 | " + " | ".join(aspects) + " |",
        "| --- | " + " | ".join("---" for _ in aspects) + " |",
    ]
    for target in targets:
        answer_text = by_target.get(target, "")
        question_text = question_by_target.get(target, "")
        cells = [
            _extract_aspect_line(answer_text, aspect, question_text)
            for aspect in aspects
        ]
        lines.append("| " + target + " | " + " | ".join(cells) + " |")
    return "\n".join(lines) + "\n"


# --------------------------------------------------------- pure: questions


def build_questions_for_targets(
    targets, aspects, comparison_target=None, max_asks=None, fallback_question=""
):
    """The `ask()` questions for one run (ADR-0063 Phase 109d C3). One
    question per target (`- <aspect>: <fact (citation)>` / `未確認` for each
    aspect), plus one comparison summary question if `comparison_target` is
    given and there is still room under `max_asks` -- targets are taken from
    the front and the summary is dropped first when the budget is tight. No
    targets at all: a single fallback question (the 109c structured prompt,
    supplied by the Rust side, unchanged)."""
    targets = [str(t).strip() for t in (targets or []) if str(t or "").strip()]
    max_asks = max(0, int(max_asks if max_asks is not None else DEFAULT_MAX_ASKS))
    if not targets:
        return [{"id": "q1", "target": None, "question": fallback_question}]

    aspects = [str(a).strip() for a in (aspects or []) if str(a or "").strip()]
    aspect_list = "、".join(aspects)
    limited = targets[:max_asks]
    questions = []
    for index, target in enumerate(limited, start=1):
        text = (
            "%s について、次の観点を提示された文献の範囲で答えよ: %s。"
            "文献に無い観点は『未確認』と書け。各事実に引用を付けよ。" % (target, aspect_list)
        )
        questions.append({"id": "t%d" % index, "target": target, "question": text})

    remaining = max_asks - len(questions)
    comparison_target = str(comparison_target).strip() if comparison_target else ""
    if remaining > 0 and comparison_target:
        summary_text = (
            "対象ごとに %s と『公平比較可能』か『背景比較のみ』かを分類し理由を1行で述べよ。\n"
            "対象: %s" % (comparison_target, "、".join(limited))
        )
        questions.append({"id": "summary", "target": None, "question": summary_text})
    return questions


# ------------------------------------------------------------------ retries


RETRY_DELAYS = (2.0, 4.0, 8.0)
_TRANSIENT_ERROR_RE = re.compile(
    r"\b(429|502|503)\b|connection\s*(reset|refused|aborted|error)|timed?\s*out",
    re.IGNORECASE,
)


def is_transient_error(exc):
    """Whether `exc` (an exception raised by `ask()`) looks like one of the
    llm-proxy's transient failure modes (ADR-0063 Phase 109d C1: the same
    503/429/502/connection-drop set Phase 109b already retries for LDR)."""
    return bool(_TRANSIENT_ERROR_RE.search(str(exc)))


def ask_with_retries(perform, sleep=None, delays=RETRY_DELAYS):
    """Call `perform()` (one `ask()` call) up to `len(delays) + 1` times
    total, 2s/4s/8s backoff between attempts, but only for a transient
    failure (`is_transient_error`); anything else propagates immediately.
    `sleep` is looked up from `time.sleep` at call time so a test can
    monkeypatch it without any real waiting."""
    if sleep is None:
        sleep = time.sleep
    last_exc = None
    for delay in (*delays, None):
        try:
            return perform()
        except Exception as exc:  # noqa: BLE001 - re-raised below when not transient/last
            if delay is None or not is_transient_error(exc):
                raise
            last_exc = exc
            sleep(delay)
    raise last_exc  # pragma: no cover -- the loop above always returns or raises


# -------------------------------------------------------------- pure: settings


def resolve_settings_path(settings_dir, settings_name):
    """The absolute `<settings_dir>/<settings_name>.json` path (ADR-0063
    Phase 109e), the same one the adapter's `split_settings_path`
    (crates/task-worker/src/paperqa.rs) builds from `[adapters.paperqa]
    settings`. Normally the adapter already sends this as `settings_path` in
    the input JSON; this pure re-derivation exists so (a) a test can check
    the two sides agree without installing `paperqa`, and (b) `load_settings`
    still has something to try if `settings_path` is ever missing from the
    input. `None` when there isn't enough to build one -- no `settings_name`,
    or a bare name with no directory (that case only has
    `Settings.from_name` to fall back to, same as before Phase 109e)."""
    if not settings_name or not settings_dir:
        return None
    name = settings_name[:-5] if settings_name.endswith(".json") else settings_name
    if not name:
        return None
    return os.path.join(settings_dir, name + ".json")


class SettingsResolutionError(Exception):
    """Raised by `load_settings` when settings could not be found by either
    lookup path (ADR-0063 Phase 109e): neither `settings_path` names an
    existing file, nor does `Settings.from_name(settings_name)` find
    anything. `str(exc)` lists every path tried, for a human to act on
    (fix `[adapters.paperqa] settings` or create the file); `main` turns
    this into exit 2, which the adapter treats as non-retryable."""


def load_settings(payload):
    """The `paperqa.Settings` for this run (ADR-0063 Phase 109e). Assumes
    `paperqa` is already imported -- the caller guards that separately with
    its own ImportError -> exit 3 handling.

    1. If `settings_path` (given directly in `payload`, or rebuilt from
       `settings_dir` + `settings_name` via `resolve_settings_path`) names a
       file that exists, read and validate it **directly**. Production
       `Settings.from_name` (paperqa==2026.8.12) looks up
       `pqa_directory("settings")` (`~/.pqa/settings/`), which does not
       honor `PQA_SETTINGS_DIR` -- that lookup can never find a settings
       file that lives anywhere else, which is exactly the bug this phase
       fixes (observed in production, ADR-0063 Phase 109e). We redo the
       same two steps `from_name` performs internally (validate the raw
       JSON into a throwaway `Settings`, then rebuild from its
       `model_dump()` so whatever defaults `Settings.__init__` normally
       fills in still apply) so `agent.index.*` and every other field behave
       the same as the old `-s <name>` CLI flag / `from_name` path did.
    2. Otherwise, if `settings_name` is given, fall back to
       `Settings.from_name(settings_name)` -- still the only way to reach
       paperqa's own bundled config names (e.g. `"high_quality"`).
    3. Otherwise (no `settings_name` either), `Settings()` (paperqa's
       built-in defaults) -- there was never anything to look up.

    Raises `SettingsResolutionError` only when `settings_name` was given but
    neither step above found anything.
    """
    from paperqa import Settings

    settings_dir = payload.get("settings_dir") or None
    settings_name = payload.get("settings_name") or None
    settings_path = payload.get("settings_path") or resolve_settings_path(
        settings_dir, settings_name
    )

    tried = []
    if settings_path:
        tried.append(settings_path)
        if os.path.isfile(settings_path):
            with open(settings_path, "r", encoding="utf-8") as handle:
                raw_json = handle.read()
            tmp = Settings.model_validate_json(raw_json)
            return Settings(**tmp.model_dump())

    if not settings_name:
        return Settings()

    try:
        return Settings.from_name(settings_name)
    except FileNotFoundError as exc:
        tried.append("Settings.from_name(%r): %s" % (settings_name, exc))
        raise SettingsResolutionError(
            "could not resolve PaperQA settings %r; looked at: %s"
            % (settings_name, "; ".join(tried))
        ) from exc


# --------------------------------------------------------------------- main


def make_progress_printer():
    def progress(text):
        collapsed = " ".join(str(text).split())
        if collapsed:
            print("progress: %s" % collapsed, flush=True)

    return progress


def write_json(path, value):
    directory = os.path.dirname(path) or "."
    os.makedirs(directory, exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(value, handle, indent=2, ensure_ascii=False)
        handle.write("\n")


def answer_from_session(session):
    return _get(session, "formatted_answer") or _get(session, "answer") or ""


def ask_one(ask_fn, settings, question):
    """One question -> one answer dict for `output_path["answers"]`
    (ADR-0063 Phase 109d C1). Never raises: a failure (after retries) is
    recorded as `{"error": ...}` with an empty answer/contexts so the other
    questions still get a chance to run."""
    try:
        response = ask_with_retries(lambda: ask_fn(question["question"], settings=settings))
    except Exception as exc:  # noqa: BLE001 - recorded, not re-raised
        return {
            "id": question.get("id"),
            "target": question.get("target"),
            "question": question.get("question"),
            "answer": "",
            "has_successful_answer": False,
            "contexts": [],
            "references": "",
            "cost": None,
            "token_counts": None,
            "error": str(exc),
        }
    session = _get(response, "session", response)
    return {
        "id": question.get("id"),
        "target": question.get("target"),
        "question": question.get("question"),
        "answer": answer_from_session(session),
        "has_successful_answer": bool(_get(session, "has_successful_answer", False)),
        "contexts": flatten_contexts(_get(session, "contexts") or [], question.get("id")),
        "references": _get(session, "references") or "",
        "cost": _get(session, "cost"),
        "token_counts": _get(session, "token_counts"),
    }


def main():
    argv = sys.argv[1:]
    if not argv:
        print("usage: paperqa_ask.py <input.json>", file=sys.stderr)
        return 2

    with open(argv[0], "r", encoding="utf-8") as handle:
        payload = json.load(handle)

    output_path = payload["output_path"]
    questions = build_questions_for_targets(
        payload.get("targets"),
        payload.get("aspects"),
        payload.get("comparison_target"),
        payload.get("max_asks"),
        payload.get("fallback_question") or "",
    )

    try:
        import paperqa  # noqa: F401 - import guarded per ADR-0063 Phase 109d C1
        from paperqa import ask
    except ImportError as exc:
        write_json(output_path, {"error": "paperqa not importable: %s" % exc, "answers": []})
        print("paperqa not importable: %s" % exc, file=sys.stderr)
        return 3

    try:
        settings = load_settings(payload)
    except SettingsResolutionError as exc:
        write_json(output_path, {"error": str(exc), "answers": []})
        print(str(exc), file=sys.stderr)
        return 2
    settings.agent.index.paper_directory = payload.get("paper_directory") or ""
    settings.agent.index.index_directory = payload.get("index_directory") or ""
    index_name = payload.get("index_name") or None
    if index_name:
        settings.agent.index.name = index_name
    model = payload.get("model") or None
    if model:
        settings.llm = model

    progress = make_progress_printer()
    answers = []
    for question in questions:
        progress("asking: %s" % (question.get("target") or question.get("id")))
        answer = ask_one(ask, settings, question)
        if answer.get("error"):
            progress("ask failed for %s: %s" % (question.get("id"), answer["error"]))
        answers.append(answer)

    table = build_target_aspect_table(payload.get("targets"), payload.get("aspects"), answers)
    write_json(output_path, {"answers": answers, "target_aspect_table": table})
    return 0


if __name__ == "__main__":
    sys.exit(main())
