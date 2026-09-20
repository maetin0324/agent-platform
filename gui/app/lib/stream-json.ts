/**
 * `stdout.jsonl`（各 run のワーカー標準出力）の 1 行を、表示上の見出し分けのためだけに分類する純粋関数。
 * docs/adr/0006 D2: claude-code（`crates/task-worker/src/claude_code.rs`）と codex（`crates/task-worker/src/codex.rs`）の
 * `handle_line` を読解して型だけ揃えたもので、celeris 側の分類規則やリトライ可否・成否の意味づけを再実装するのではない。
 * `is_error` 等は celeris が付けた値をそのまま見せるだけ。celeris 独自ワーカープロトコル（fake ワーカーの `progress`/`done` 等）は
 * claude-code/codex のどちらでもないため全て `raw` になる。
 */

export type FormattedLine =
  | { kind: "utterance"; text: string }
  | { kind: "tool"; label: string; detail?: string }
  | { kind: "result"; text: string; isError: boolean }
  | { kind: "raw"; text: string };

function raw(line: string): FormattedLine {
  return { kind: "raw", text: line };
}

function classifyClaudeCode(value: Record<string, unknown>, line: string): FormattedLine | undefined {
  if (value.type === "assistant") {
    const message = value.message;
    if (typeof message !== "object" || message === null) return undefined;
    const content = (message as Record<string, unknown>).content;
    if (!Array.isArray(content)) return undefined;
    for (const item of content) {
      if (typeof item !== "object" || item === null) continue;
      const it = item as Record<string, unknown>;
      if (it.type === "text" && typeof it.text === "string") {
        return { kind: "utterance", text: it.text };
      }
      if (it.type === "tool_use" && typeof it.name === "string") {
        const detail = it.input === undefined ? undefined : JSON.stringify(it.input);
        return detail === undefined ? { kind: "tool", label: it.name } : { kind: "tool", label: it.name, detail };
      }
    }
    return undefined;
  }
  if (value.type === "result") {
    const isError = typeof value.is_error === "boolean" ? value.is_error : value.subtype !== "success";
    const text = typeof value.result === "string" ? value.result : String(value.subtype ?? line);
    return { kind: "result", text, isError };
  }
  return undefined;
}

function codexAgentMessageText(item: Record<string, unknown>, line: string): string {
  if (typeof item.text === "string") return item.text;
  if (typeof item.message === "string") return item.message;
  if (typeof item.content === "string") return item.content;
  return line;
}

function codexErrorText(error: unknown, fallback: string): string {
  if (typeof error === "string") return error;
  if (typeof error === "object" && error !== null && typeof (error as Record<string, unknown>).message === "string") {
    return (error as Record<string, unknown>).message as string;
  }
  return fallback;
}

function summarizeUsage(usage: unknown): string | undefined {
  if (typeof usage !== "object" || usage === null) return undefined;
  const u = usage as Record<string, unknown>;
  const input = u.input_tokens;
  const output = u.output_tokens;
  if (typeof input !== "number" && typeof output !== "number") return undefined;
  return `turn.completed (input_tokens=${input ?? "?"}, output_tokens=${output ?? "?"})`;
}

function classifyCodex(value: Record<string, unknown>, line: string): FormattedLine | undefined {
  const type = value.type;
  if (typeof type !== "string") return undefined;
  if (type.startsWith("item.")) {
    const item = value.item;
    if (typeof item !== "object" || item === null) return undefined;
    const it = item as Record<string, unknown>;
    if (it.type === "agent_message") {
      return { kind: "utterance", text: codexAgentMessageText(it, line) };
    }
    if (typeof it.type === "string") {
      return { kind: "tool", label: it.type, detail: JSON.stringify(it) };
    }
    return undefined;
  }
  if (type === "turn.completed") {
    return { kind: "result", text: summarizeUsage(value.usage) ?? "turn.completed", isError: false };
  }
  if (type === "turn.failed") {
    return { kind: "result", text: codexErrorText(value.error, "turn.failed"), isError: true };
  }
  if (type === "error") {
    return { kind: "result", text: typeof value.message === "string" ? value.message : "error", isError: true };
  }
  if (type === "thread.started") {
    return raw(line);
  }
  return undefined;
}

/**
 * `stdout.jsonl` の 1 行を分類する。claude-code / codex のどちらの形式にも当てはまらない行
 * （不正な JSON、`type` の無い行、未知の `type`、celeris 独自ワーカープロトコルの行）は全て `raw`。
 */
export function classifyStreamJsonLine(line: string): FormattedLine {
  let parsed: unknown;
  try {
    parsed = JSON.parse(line);
  } catch {
    return raw(line);
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    return raw(line);
  }
  const value = parsed as Record<string, unknown>;
  if (typeof value.type !== "string") {
    return raw(line);
  }
  return classifyClaudeCode(value, line) ?? classifyCodex(value, line) ?? raw(line);
}
