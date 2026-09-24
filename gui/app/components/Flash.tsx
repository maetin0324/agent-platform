import { Link } from "react-router";
import type {
  AccountOpOutcome,
  ActionError,
  ApprovalOpOutcome,
  NotifyTestOutcome,
  OrgOpOutcome,
  ProjectOpOutcome,
  ProviderActionResult,
  ReleasePromoteOutcome,
  ReportOpOutcome,
  RetryOutcome,
  SecretActionResult,
  StandingRuleOpOutcome,
  TaskCommentOutcome,
  TaskEditOutcome,
  TaskReopenOutcome,
  TaskRereviewOutcome,
  TransitionOutcome,
} from "~/celeris/action-types";
import { Alert } from "~/components/ui/misc";
import { cancelledCountLabel, commentEffectMessage, taskFieldLabel } from "~/lib/labels";
import type { PromoteFlashState } from "~/lib/releases";

/**
 * action の結果表示（docs/DESIGN.md §6.3 の 2「`TransitionResult` を flash に載せる」、docs/adr/0005 D2）。
 * クッキーのセッションは使わず、action が返した data（`actionData` / `fetcher.data`）をそのまま描く。
 * 409 は「状態が変わりました」（loader の再検証で画面は最新になる）、422 は celeris の文言そのまま。
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

/**
 * `POST /tasks/{id}/retry`（Phase 31。実機の事故、2026-09-18）の結果。成功したら画面は新しいタスクへ
 * 遷移する（呼び出し側が `useEffect` + `useNavigate` で行う）ので、ここは遷移するまでの一瞬だけ見える
 * 「新しいタスクを作りました」の案内とリンク。
 */
export function RetryFlash({ outcome }: { outcome: RetryOutcome | undefined | null }) {
  if (!outcome) return null;
  if (outcome.ok) {
    return (
      <Alert role="status" data-testid="flash" data-flash-kind="ok" tone="success" className="my-2">
        <p>
          やり直す: 新しいタスク{" "}
          <Link
            to={`/tasks/${outcome.result.task_id}`}
            className="underline underline-offset-2"
            data-testid="flash-retry-task-id"
          >
            {outcome.result.task_id}
          </Link>{" "}
          を作りました。移動します…
        </p>
        {(outcome.result.rewired ?? []).length > 0 && (
          <p data-testid="flash-retry-rewired">
            depends_on を張り替えたタスク:{" "}
            {(outcome.result.rewired ?? []).map((id) => (
              <Link key={id} to={`/tasks/${id}`} className="mr-2 underline">
                {id}
              </Link>
            ))}
          </p>
        )}
      </Alert>
    );
  }
  return <ErrorFlash error={outcome.error} />;
}

/**
 * タスクの編集の結果（ADR-0044 D1、Phase 53）。**celeris が返した `fields`（実際に変わった項目）**を
 * そのまま出す。`running` / `reviewing` のタスクは走っている run を止めないので、その旨を添える
 * （止めたいときはコメント（D2）か取り消し）。
 */
export function TaskEditFlash({
  outcome,
  runningNote = false,
}: {
  outcome: TaskEditOutcome | undefined | null;
  runningNote?: boolean;
}) {
  if (!outcome) return null;
  if (!outcome.ok) return <ErrorFlash error={outcome.error} />;
  const { fields } = outcome.result;
  if (fields.length === 0) {
    return (
      <Alert role="status" data-testid="flash" data-flash-kind="ok" tone="info" className="my-2">
        <p data-testid="flash-task-edit">変わった項目はありません。</p>
      </Alert>
    );
  }
  return (
    <Alert role="status" data-testid="flash" data-flash-kind="ok" tone="success" className="my-2">
      <p data-testid="flash-task-edit">変えました: {fields.map(taskFieldLabel).join("・")}</p>
      {runningNote && (
        <p data-testid="flash-task-edit-running">
          いま走っている run は止めていません（この変更は次の run から効きます）。すぐ止めたいときは
          タイムラインにコメントしてください。
        </p>
      )}
    </Alert>
  );
}

/**
 * タスクへのコメントの結果（ADR-0044 D2、Phase 53）。**何が起きたか**（`effect`）を必ず言う:
 * 走っていた run を止めたのか、質問への回答になったのか、記録しただけなのか。
 * 状態が動いたときは `transition` の from → to も添える。
 */
export function TaskCommentFlash({ outcome }: { outcome: TaskCommentOutcome | undefined | null }) {
  if (!outcome) return null;
  if (!outcome.ok) return <ErrorFlash error={outcome.error} />;
  const { effect, transition, can_reopen } = outcome.result;
  return (
    <Alert
      role="status"
      data-testid="flash"
      data-flash-kind="ok"
      data-flash-effect={effect}
      tone={effect === "interrupted" ? "warning" : "success"}
      className="my-2"
    >
      <p data-testid="flash-comment-effect">{commentEffectMessage(effect)}</p>
      {transition && (
        <p data-testid="flash-comment-transition">
          <span data-testid="flash-from">{transition.from}</span> → <span data-testid="flash-to">{transition.to}</span>
          （reason: {transition.reason}）
        </p>
      )}
      {effect === "terminal" && !can_reopen && (
        <p data-testid="flash-comment-no-reopen">
          中止したタスクは再開できません（worktree を消してあります）。「やり直す」で複製してください。
        </p>
      )}
    </Alert>
  );
}

/** 終端のタスクの再開の結果（ADR-0044 D2、Phase 53）。 */
export function TaskReopenFlash({ outcome }: { outcome: TaskReopenOutcome | undefined | null }) {
  if (!outcome) return null;
  if (!outcome.ok) return <ErrorFlash error={outcome.error} />;
  const { result } = outcome;
  return (
    <Alert role="status" data-testid="flash" data-flash-kind="ok" tone="success" className="my-2">
      <p data-testid="flash-task-reopen">
        再開しました: <span data-testid="flash-from">{result.from}</span> →{" "}
        <span data-testid="flash-to">{result.to}</span>（reason: {result.reason}）
      </p>
    </Alert>
  );
}

/** 既存成果の再判定の結果（ADR-0070 D2、Phase 116）。 */
export function TaskRereviewFlash({ outcome }: { outcome: TaskRereviewOutcome | undefined | null }) {
  if (!outcome) return null;
  if (!outcome.ok) return <ErrorFlash error={outcome.error} />;
  const { result } = outcome;
  return (
    <Alert role="status" data-testid="flash" data-flash-kind="ok" tone="success" className="my-2">
      <p data-testid="flash-task-rereview">
        再レビューを始めました: <span data-testid="flash-from">{result.from}</span> →{" "}
        <span data-testid="flash-to">{result.to}</span>（reason: {result.reason}）
      </p>
    </Alert>
  );
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
          管理系 API はトークンが必須です（ADR-GUI-0012 D1）。<code>CELERIS_API_TOKEN_FILE</code> を celeris の{" "}
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
  // login_code は HTTP としては 200（celeris の 3.34）だが、result.result が "failed"（誤ったコード等）のことがある。
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

const SECRET_OP_LABEL: Record<string, string> = { put: "保存", delete: "削除" };

/**
 * `/accounts` の API キー節の action の結果（ADR-0030 D4）。値は載せない。続けて呼んだ `POST /reload` の結果も出す
 * （`ProviderActionFlash` と同じ作り）。
 */
export function SecretActionFlash({ result }: { result: SecretActionResult | undefined | null }) {
  if (!result) return null;
  const { op, reload } = result;
  if (!op.ok) return <ErrorFlash error={op.error} />;
  return (
    <div className="my-2 space-y-2" data-testid="secret-action-flash">
      <Alert role="status" data-testid="flash" data-flash-kind="ok" tone="success">
        <p data-testid="flash-secret-op">
          {SECRET_OP_LABEL[op.op] ?? op.op}: <span className="font-mono">{op.id}</span>
        </p>
      </Alert>
      {reload &&
        (reload.ok ? (
          <Alert data-testid="flash-reload" data-flash-kind="ok" tone="success">
            reload: 反映しました（次の tick から）。
          </Alert>
        ) : (
          <Alert data-testid="flash-reload" data-flash-kind="error" tone="danger">
            {op.op === "put" ? "値は保存されましたが" : "削除はできましたが"}
            、反映（reload）に失敗しました: {reload.error.detail}
          </Alert>
        ))}
    </div>
  );
}

const ORG_OP_LABEL: Record<string, string> = { create: "追加", patch: "変更", delete: "削除" };

/**
 * 「組織」画面の action の結果（ADR-0033 D1）。DB が正なので `reload` は無い
 * （`ProviderActionFlash` / `SecretActionFlash` との対比。`ClusterConnectOutcome` の扱いと同じ）。
 */
export function OrgActionFlash({ outcome }: { outcome: OrgOpOutcome | undefined | null }) {
  if (!outcome) return null;
  if (!outcome.ok) return <ErrorFlash error={outcome.error} />;
  return (
    <Alert role="status" data-testid="flash" data-flash-kind="ok" tone="success" className="my-2">
      <p data-testid="flash-org-op">
        {ORG_OP_LABEL[outcome.op] ?? outcome.op}: <span className="font-mono">{outcome.id}</span>
      </p>
    </Alert>
  );
}

const PROJECT_OP_LABEL: Record<string, string> = {
  project_status: "案件の状態を変更",
  project_workspace: "作業場所を変更",
  milestone_create: "途中目標を追加",
  milestone_status: "途中目標の状態を変更",
  project_plan: "分解を CoS に頼みました",
  // 案件のリポジトリ（ADR-0043 D1、Phase 52 / G16）。
  repo_create: "リポジトリを追加",
  repo_patch: "リポジトリを変更",
  repo_primary: "主なリポジトリを変更",
  repo_delete: "リポジトリを削除",
  // ADR-0044 D1（Phase 53）: 人が作ったタスクは `ready` で始まる（Go を挟まない）。
  task_create: "タスクを追加しました（待機中で始まります）",
  // ADR-0044 D6（Phase 55 / G19）: 中止・一時停止・アーカイブ。
  project_cancel: "案件を中止しました",
  project_pause: "案件を一時停止しました",
  project_resume: "案件を再開しました",
  project_archive: "案件をアーカイブしました",
  project_unarchive: "アーカイブを解除しました",
  milestone_cancel: "途中目標を中止しました",
  milestone_pause: "途中目標を一時停止しました",
  milestone_resume: "途中目標を再開しました",
};

/** 中止・一時停止・アーカイブの `op`（`lifecycle` を持つ結果かどうかの判定に使う）。 */
const LIFECYCLE_OPS: readonly string[] = [
  "project_cancel",
  "project_pause",
  "project_resume",
  "project_archive",
  "project_unarchive",
  "milestone_cancel",
  "milestone_pause",
  "milestone_resume",
];

/** 途中目標の判定（`ok`/`discuss`/`ng`）ごとの文言（ADR-0038 D2/D3、Phase 41 / G13j）。 */
const MILESTONE_DECIDE_LABEL: Record<string, string> = {
  ok: "達成にして、次の途中目標を承認し、分解を CoS に頼みました",
  discuss: "議論を CoS に伝えました。Console で返事を待ってください",
  ng: "達成にせず、再設計を CoS に頼みました",
};

/**
 * 「案件」画面の action の結果（ADR-0033 D2）。案件・途中目標の操作は管理系ではない（3.42〜3.49 の前書き）。
 * 「この方針で進める」（`project_plan`、§3.61）は 202 なので、待たずに「頼みました」と出し、
 * その裏方のタスクへのリンクを添える（仕事の木は SSE の再検証で増えていく）。
 * 途中目標の判定（`milestone_decide`、§3.63）も同じく 202 で、`decided.decision` ごとの文言と、
 * `ok` で分解が起きたときは `plan_task_id` へのリンクを出す（`discuss` は呼び出し側が秘書の対話画面へ
 * 遷移するので、ここは遷移するまでの一瞬だけ見える）。
 */
export function ProjectActionFlash({ outcome }: { outcome: ProjectOpOutcome | undefined | null }) {
  if (!outcome) return null;
  if (!outcome.ok) return <ErrorFlash error={outcome.error} />;
  if (outcome.op === "milestone_decide") {
    const { decided } = outcome;
    return (
      <Alert role="status" data-testid="flash" data-flash-kind="ok" tone="success" className="my-2">
        <p data-testid="flash-milestone-decide">
          {MILESTONE_DECIDE_LABEL[decided.decision] ?? decided.decision}
          {decided.plan_task_id && (
            <>
              （
              <Link
                to={`/tasks/${decided.plan_task_id}`}
                data-testid="flash-milestone-plan-task"
                className="underline underline-offset-2"
              >
                裏方のタスク
              </Link>
              ）
            </>
          )}
        </p>
      </Alert>
    );
  }
  // 中止・一時停止・アーカイブ（ADR-0044 D6、Phase 55 / G19）。中止のときだけ、**celeris が返した**
  // `cancelled_tasks` / `cancelled_milestones` の件数を添える（連鎖は GUI で数え直さない）。
  if (LIFECYCLE_OPS.includes(outcome.op) && "lifecycle" in outcome) {
    const { lifecycle } = outcome;
    const cancelledTasks = lifecycle.cancelled_tasks ?? [];
    const cancelledMilestones = "cancelled_milestones" in lifecycle ? (lifecycle.cancelled_milestones ?? []) : null;
    const cancelling = outcome.op === "project_cancel" || outcome.op === "milestone_cancel";
    return (
      <Alert
        role="status"
        data-testid="flash"
        data-flash-kind="ok"
        data-flash-op={outcome.op}
        tone={cancelling ? "warning" : "success"}
        className="my-2"
      >
        <p data-testid="flash-project-op">{PROJECT_OP_LABEL[outcome.op] ?? outcome.op}</p>
        {cancelling && (
          <p data-testid="flash-lifecycle-cancelled">
            {cancelledCountLabel(cancelledTasks.length, cancelledMilestones?.length)}
          </p>
        )}
        {cancelling && cancelledTasks.length > 0 && (
          <ul className="list-disc space-y-0.5 pl-5">
            {cancelledTasks.map((ref) => (
              <li key={ref.id}>
                <Link to={`/tasks/${ref.id}`} data-testid="flash-lifecycle-task" className="underline">
                  {ref.title}
                </Link>
              </li>
            ))}
          </ul>
        )}
      </Alert>
    );
  }
  return (
    <Alert role="status" data-testid="flash" data-flash-kind="ok" tone="success" className="my-2">
      <p data-testid="flash-project-op">
        {PROJECT_OP_LABEL[outcome.op] ?? outcome.op}
        {outcome.op === "task_create" && (
          <>
            （
            <Link
              to={`/tasks/${outcome.task.id}`}
              data-testid="flash-task-create-link"
              className="underline underline-offset-2"
            >
              {outcome.task.title}
            </Link>
            ）
          </>
        )}
        {outcome.op === "project_plan" && (
          <>
            （
            <Link
              to={`/tasks/${outcome.accepted.task_id}`}
              data-testid="flash-project-plan-task"
              className="underline underline-offset-2"
            >
              裏方のタスク
            </Link>
            ）。仕事の木がこれから増えていきます。
          </>
        )}
      </p>
    </Alert>
  );
}

const REPORT_OP_LABEL: Record<string, string> = {
  reports_read: "既読にしました",
  reports_notified: "通知を記録しました",
};

/**
 * 「報告」画面の action の結果（ADR-0033 D3）。`reports_notified` は画面が出ていなくても
 * `NotificationsWatcher` が呼ぶので、そちらでは表示しない（ここは `/reports` の `fetcher.data` 用）。
 */
export function ReportActionFlash({ outcome }: { outcome: ReportOpOutcome | undefined | null }) {
  if (!outcome) return null;
  if (!outcome.ok) return <ErrorFlash error={outcome.error} />;
  return (
    <Alert role="status" data-testid="flash" data-flash-kind="ok" tone="success" className="my-2">
      <p data-testid="flash-report-op">
        {REPORT_OP_LABEL[outcome.op] ?? outcome.op}
        {outcome.op === "reports_read" && <>（{outcome.result.updated} 件）</>}
      </p>
    </Alert>
  );
}

/**
 * Discord のテスト送信の結果（ADR-0037 D4、docs/celeris-api-v1.md §3.65）。`outcome.ok` は action（HTTP）が
 * 成功したかどうかで、成功していても `result.ok` が `false`（送り先が 404 を返した等）はありうる。
 * `detail` は種別だけの短い文（URL・ホスト名は含まない。celeris 側の規律）。
 */
export function NotifyTestFlash({ outcome }: { outcome: NotifyTestOutcome | undefined | null }) {
  if (!outcome) return null;
  if (!outcome.ok) return <ErrorFlash error={outcome.error} />;
  const { result } = outcome;
  return (
    <Alert
      role="status"
      data-testid="flash"
      data-flash-kind={result.ok ? "ok" : "error"}
      tone={result.ok ? "success" : "danger"}
      className="my-2"
    >
      <p data-testid="flash-notify-test">
        {result.ok ? "テスト送信: 届きました" : "テスト送信: 届きませんでした"}
        {result.detail && <>（{result.detail}）</>}
      </p>
    </Alert>
  );
}

/**
 * 「リリース」画面（`/releases`、Phase G14。ADR-0040 D6）の upgrade の結果。202 は
 * 「`promote.sh` を起こした」だけで、**切り替えが終わったわけではない**ことを必ず書く
 * （旧 celeris はこのあと `draining` になって手元の run を見終わってから終わる。ADR-0040 D4）。
 *
 * バグ報告 2026-09-21 その 2: 押したあと、成功したのか失敗したのか、現行のコミットハッシュが
 * 変わったのか画面から分からなかった。`state`（`~/lib/releases.ts` の `promoteFlashState`）で
 * 「始めました」→「完了しました」に文言を差し替える（失敗は `release-promote-failed` の
 * 赤いバナーが別に出すので、ここでは隠す）。
 */
export function ReleasePromoteFlash({
  outcome,
  state,
}: {
  outcome: ReleasePromoteOutcome | undefined | null;
  state: PromoteFlashState;
}) {
  if (!outcome) return null;
  if (!outcome.ok) return <ErrorFlash error={outcome.error} />;
  if (state === "hidden") return null;
  if (state === "succeeded") {
    return (
      <Alert
        role="status"
        data-testid="flash"
        data-flash-kind="ok"
        tone="success"
        title="upgrade が完了しました"
        className="my-2"
      >
        <p data-testid="flash-release-promote">
          {outcome.sha12} への切り替えが完了しました。現行のリリースになっています。
        </p>
      </Alert>
    );
  }
  return (
    <Alert
      role="status"
      data-testid="flash"
      data-flash-kind="ok"
      tone="success"
      title="upgrade を始めました"
      className="my-2"
    >
      <p data-testid="flash-release-promote">
        {outcome.sha12} への切り替えを始めました。完了は下の「切り替えの進行」で確認してください （この画面は 2
        秒ごとに自動で読み直します）。
      </p>
    </Alert>
  );
}

const APPROVAL_DECISION_LABEL: Record<string, string> = {
  once: "今回だけ",
  standing: "今後ずっと",
  denied: "認めない",
};

/**
 * 「認可」画面の action の結果（SPEC §3.6、ADR-0033 D5）: `POST /approvals/{id}/decide`。
 * `standing` で答えたときは、続けて足された永続の認可があることも出す。
 */
export function ApprovalActionFlash({ outcome }: { outcome: ApprovalOpOutcome | undefined | null }) {
  if (!outcome) return null;
  if (!outcome.ok) return <ErrorFlash error={outcome.error} />;
  const decision = outcome.result.approval.decision;
  return (
    <Alert role="status" data-testid="flash" data-flash-kind="ok" tone="success" className="my-2">
      <p data-testid="flash-approval-op">
        答えました: {decision ? (APPROVAL_DECISION_LABEL[decision] ?? decision) : "決定"}
        {outcome.result.standing_rule && <>（永続の認可に追加しました）</>}
      </p>
    </Alert>
  );
}

const STANDING_RULE_OP_LABEL: Record<string, string> = { create: "追加", delete: "削除" };

/** 「永続の認可」の追加・削除の結果（ADR-0033 D5、docs/celeris-api-v1.md §3.59〜3.60）。 */
export function StandingRuleActionFlash({ outcome }: { outcome: StandingRuleOpOutcome | undefined | null }) {
  if (!outcome) return null;
  if (!outcome.ok) return <ErrorFlash error={outcome.error} />;
  return (
    <Alert role="status" data-testid="flash" data-flash-kind="ok" tone="success" className="my-2">
      <p data-testid="flash-standing-rule-op">{STANDING_RULE_OP_LABEL[outcome.op] ?? outcome.op}</p>
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
