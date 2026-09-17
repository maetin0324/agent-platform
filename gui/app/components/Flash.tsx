import { Link } from "react-router";
import { Alert } from "~/components/ui/misc";
import type { AccountOpOutcome, ActionError, ProviderActionResult, TransitionOutcome } from "~/taskd/action-types";

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
      {error.code === "unauthorized" && (
        <p data-testid="flash-unauthorized-hint">
          管理系 API はトークンが必須です（ADR-GUI-0012 D1）。<code>TASKD_API_TOKEN_FILE</code> を taskd の{" "}
          <code>[api] token_file</code> と同じ内容にして GUI を再起動してください。
        </p>
      )}
      {error.code === "login_code_not_supported" && (
        <p data-testid="flash-login-code-not-supported">
          このアカウントは codex（デバイス認証）のため、コードをここに貼り付けることはできません（ADR-0025
          D5）。ログイン開始時に表示された URL を別のデバイスで開き、その画面でコードを入力してください。
        </p>
      )}
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

const PROVIDER_OP_LABEL: Record<string, string> = { create: "追加", patch: "変更", delete: "削除", check: "疎通確認" };

/**
 * `/providers` の action の結果（ADR-GUI-0012 D2）: 追加・変更・削除・疎通確認 1 件と、
 * 追加・変更・削除のときは続けて呼んだ `POST /reload` の結果の両方を出す。
 */
export function ProviderActionFlash({ result }: { result: ProviderActionResult | undefined | null }) {
  if (!result) return null;
  const { op, reload } = result;
  if (!op.ok) return <ErrorFlash error={op.error} />;
  return (
    <div className="my-2 space-y-2" data-testid="provider-action-flash">
      <Alert role="status" data-testid="flash" data-flash-kind="ok" tone="success">
        <p data-testid="flash-provider-op">
          {PROVIDER_OP_LABEL[op.op] ?? op.op}: <span className="font-mono">{op.id}</span>
          {op.op === "check" && (
            <>
              {" → "}
              <span data-testid="flash-provider-check-result">{op.result.result}</span>
              {op.result.detail && <>（{op.result.detail}）</>}
            </>
          )}
        </p>
      </Alert>
      {reload &&
        (reload.ok ? (
          <Alert data-testid="flash-reload" data-flash-kind="ok" tone="success">
            reload: 反映しました（次の tick から）。
          </Alert>
        ) : (
          <Alert data-testid="flash-reload" data-flash-kind="error" tone="danger">
            設定は書き込まれましたが、反映（reload）に失敗しました: {reload.error.detail}
          </Alert>
        ))}
    </div>
  );
}

const ACCOUNT_OP_LABEL: Record<string, string> = {
  create: "追加",
  delete: "削除",
  check: "確認",
  login_start: "ログイン開始",
  login_code: "ログイン",
  login_cancel: "ログイン中止",
};

/** `/accounts` の action の結果（ADR-GUI-0012 D3）。ログイン URL・コードの入力欄は呼び出し側（画面）が別に描く。 */
export function AccountActionFlash({ outcome }: { outcome: AccountOpOutcome | undefined | null }) {
  if (!outcome) return null;
  if (!outcome.ok) return <ErrorFlash error={outcome.error} />;
  if (outcome.op === "login_start") return null; // URL とコード入力欄は画面側が描く
  // login_code は HTTP としては 200（taskd の 3.34）だが、result.result が "failed"（誤ったコード等）のことがある。
  // その場合だけ見た目も失敗（danger）にする（ADR-GUI-0012 D3: 誤ったコードは失敗として表示）。
  const loginFailed = outcome.op === "login_code" && outcome.result.result !== "ok";
  return (
    <Alert
      role="status"
      data-testid="flash"
      data-flash-kind={loginFailed ? "error" : "ok"}
      tone={loginFailed ? "danger" : "success"}
      className="my-2"
    >
      <p data-testid="flash-account-op">
        {ACCOUNT_OP_LABEL[outcome.op] ?? outcome.op}: <span className="font-mono">{outcome.id}</span>
        {outcome.op === "check" && (
          <>
            {" → "}
            <span data-testid="flash-account-check-result">{outcome.result.result}</span>
            {outcome.result.detail && <>（{outcome.result.detail}）</>}
          </>
        )}
        {outcome.op === "login_code" && (
          <>
            {" → "}
            <span data-testid="flash-account-login-result">{outcome.result.result}</span>
            {outcome.result.detail && <>（{outcome.result.detail}）</>}
          </>
        )}
      </p>
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
