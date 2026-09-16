import { Link } from "react-router";
import { Alert } from "~/components/ui/misc";
import type { ActionError, TransitionOutcome } from "~/taskd/action-types";

/**
 * action の結果表示（docs/DESIGN.md §6.3 の 2「`TransitionResult` を flash に載せる」、docs/adr/0005 D2）。
 * クッキーのセッションは使わず、action が返した data（`actionData` / `fetcher.data`）をそのまま描く。
 * 409 は「状態が変わりました」（loader の再検証で画面は最新になる）、422 は taskd の文言そのまま。
 */
export function TransitionFlash({ outcome }: { outcome: TransitionOutcome | undefined | null }) {
  if (!outcome) return null;
  if (outcome.ok) {
    const { result } = outcome;
    const cascaded = result.cascaded ?? [];
    return (
      <Alert role="status" data-testid="flash" data-flash-kind="ok" tone="success" className="my-2">
        <p>
          <span data-testid="flash-intent">{outcome.intent}</span>:{" "}
          <Link to={`/tasks/${result.id}`} className="underline underline-offset-2">
            {result.id}
          </Link>{" "}
          <span data-testid="flash-from">{result.from}</span> → <span data-testid="flash-to">{result.to}</span>（reason:{" "}
          {result.reason}）
        </p>
        {cascaded.length > 0 && (
          <p data-testid="flash-cascaded">
            伝播して cancelled になったタスク:{" "}
            {cascaded.map((ref) => (
              <Link key={ref.id} to={`/tasks/${ref.id}`} data-testid="flash-cascaded-id" className="mr-2 underline">
                {ref.id}
              </Link>
            ))}
          </p>
        )}
      </Alert>
    );
  }
  return <ErrorFlash error={outcome.error} />;
}

export function ErrorFlash({ error }: { error: ActionError | undefined | null }) {
  if (!error) return null;
  return (
    <Alert
      role="alert"
      data-testid="flash"
      data-flash-kind="error"
      data-flash-code={error.code}
      tone="danger"
      className="my-2"
    >
      {error.conflict ? (
        <p className="font-semibold" data-testid="flash-conflict">
          状態が変わりました（{error.status} {error.code}）。画面を最新の状態に更新しました。
        </p>
      ) : (
        <p className="font-semibold">
          {error.status} {error.code}
        </p>
      )}
      <p data-testid="flash-detail">{error.detail}</p>
      {error.messages.length > 1 && (
        <ul className="list-disc space-y-0.5 pl-5">
          {error.messages.map((m) => (
            <li key={m}>{m}</li>
          ))}
        </ul>
      )}
    </Alert>
  );
}

/** 422 の `errors[]` のうち特定の `field` に付いた文言を、その欄の下に出す。 */
export function FieldErrors({ error, field }: { error: ActionError | undefined | null; field: string }) {
  const messages = error?.fields[field];
  if (!messages || messages.length === 0) return null;
  return (
    <ul className="mt-1 space-y-0.5 text-xs text-danger" data-testid={`field-error-${field}`}>
      {messages.map((m) => (
        <li key={m}>{m}</li>
      ))}
    </ul>
  );
}
