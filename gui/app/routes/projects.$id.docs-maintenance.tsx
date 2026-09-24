import { useState } from "react";
import { Form, Link, useActionData, useNavigation } from "react-router";
import { type CelerisClient, getCelerisClient } from "~/celeris/client.server";
import { CelerisError, celerisErrorResponse } from "~/celeris/errors";
import { Button } from "~/components/ui/button";
import { textareaClass } from "~/components/ui/form";
import type { Route } from "./+types/projects.$id.docs-maintenance";

type MaintenanceView = { audit: unknown; proposal: unknown; policy: unknown; saved_report?: unknown };

export async function loadMaintenance(client: CelerisClient, projectId: string, signal?: AbortSignal) {
  try {
    const view = await client.get<MaintenanceView>(`/projects/${encodeURIComponent(projectId)}/docs/maintenance`, {
      signal,
    });
    return { ...view, unavailable: null as string | null };
  } catch (e) {
    if (e instanceof CelerisError && e.code === "docs_unavailable") {
      return { audit: null, proposal: null, policy: null, saved_report: null, unavailable: e.detail };
    }
    throw e;
  }
}

export async function loader({ params, request }: Route.LoaderArgs) {
  try {
    return await loadMaintenance(getCelerisClient(), params.id, request.signal);
  } catch (e) {
    throw celerisErrorResponse(e);
  }
}

export async function action({ params, request }: Route.ActionArgs) {
  return submitMaintenance(getCelerisClient(), params.id, await request.formData(), request.signal);
}

export async function submitMaintenance(
  client: CelerisClient,
  projectId: string,
  form: FormData,
  signal?: AbortSignal,
) {
  const op = String(form.get("op"));
  try {
    const body: Record<string, unknown> = { op };
    if (op === "adopt") body.policy = JSON.parse(String(form.get("policy")));
    if (op === "approve" || op === "apply") body.plan = JSON.parse(String(form.get("plan")));
    const result = await client.post<unknown>(`/projects/${encodeURIComponent(projectId)}/docs/maintenance`, body, {
      signal,
    });
    return { result, error: null };
  } catch (e) {
    return { result: null, error: e instanceof Error ? e.message : String(e) };
  }
}

export default function DocumentationMaintenance({ loaderData, params }: Route.ComponentProps) {
  const outcome = useActionData<typeof action>();
  const [showSavedReport, setShowSavedReport] = useState(false);
  const busy = useNavigation().state !== "idle";
  const taskId =
    outcome?.result && typeof outcome.result === "object" && "task_id" in outcome.result
      ? String(outcome.result.task_id)
      : null;
  if (loaderData.unavailable) {
    return (
      <div className="space-y-6">
        <Link
          className="inline-flex min-h-11 items-center text-sm font-medium text-fg-muted hover:text-fg"
          to={`/projects/${params.id}`}
        >
          ← 案件詳細
        </Link>
        <h1 className="text-xl font-semibold">文書の監査と整理</h1>
        <p>この案件では文書監査を利用できません。ローカルの Git リポジトリが必要です。</p>
        <p className="text-fg-muted">{loaderData.unavailable}</p>
      </div>
    );
  }
  return (
    <div className="space-y-6">
      <Link
        className="inline-flex min-h-11 items-center text-sm font-medium text-fg-muted hover:text-fg"
        to={`/projects/${params.id}/docs`}
      >
        ← 文書
      </Link>
      <h1 className="text-xl font-semibold">文書の監査と整理</h1>
      <p>
        監査はコミット済み文書を読み取ります。分類は根拠付きの候補です。整理案の承認はその内容だけに有効で、変更は隔離した作業ツリーに作成されます。
      </p>
      {outcome?.error && <p role="alert">{outcome.error}</p>}
      {taskId && (
        <Link
          className="inline-flex min-h-11 items-center text-sm font-medium text-fg-muted hover:text-fg"
          to={`/tasks/${taskId}?tab=changes`}
        >
          整理結果の差分を確認し、検証タスクを開始する
        </Link>
      )}
      {outcome?.result != null && (
        <pre className="overflow-auto whitespace-pre-wrap">{JSON.stringify(outcome.result, null, 2)}</pre>
      )}
      <section className="space-y-3">
        <h2 className="font-semibold">監査結果</h2>
        <Form method="post">
          <Button name="op" value="audit" disabled={busy}>
            監査結果を保存
          </Button>
        </Form>
        <pre className="max-h-96 overflow-auto whitespace-pre-wrap">{JSON.stringify(loaderData.audit, null, 2)}</pre>
      </section>
      {loaderData.saved_report != null && (
        <details onToggle={(event) => setShowSavedReport(event.currentTarget.open)}>
          <summary className="flex min-h-11 cursor-pointer items-center">保存済み監査レポート</summary>
          {showSavedReport && (
            <pre className="max-h-96 overflow-auto whitespace-pre-wrap">
              {JSON.stringify(loaderData.saved_report, null, 2)}
            </pre>
          )}
        </details>
      )}
      <section className="space-y-3">
        <h2 className="font-semibold">具体的な整理案</h2>
        <p>
          操作・対象・置換内容を確認して承認してください。削除や移動も含め、承認なしでは適用できません。入力が変更された場合は再監査・再承認が必要です。
        </p>
        <Form method="post" className="space-y-3">
          <label htmlFor="reconcile-plan">整理案 JSON</label>
          <textarea
            id="reconcile-plan"
            name="plan"
            className={textareaClass}
            rows={14}
            defaultValue={JSON.stringify(loaderData.proposal, null, 2)}
          />
          <div className="flex flex-wrap gap-3">
            <Button name="op" value="approve" disabled={busy}>
              この整理案を承認
            </Button>
            <Button name="op" value="apply" disabled={busy}>
              承認済み案を作業ツリーに適用
            </Button>
          </div>
        </Form>
      </section>
      <section className="space-y-3">
        <h2 className="font-semibold">文書管理ポリシー</h2>
        <p>
          observe は観測のみ、conservative は既存の慣例内での整理、managed は定期監査の対象です。ポリシーは Celeris
          側に保存します。
        </p>
        <Form method="post" className="space-y-3">
          <label htmlFor="docs-policy">ポリシー JSON</label>
          <textarea
            id="docs-policy"
            name="policy"
            className={textareaClass}
            rows={10}
            defaultValue={JSON.stringify(loaderData.policy, null, 2)}
          />
          <Button name="op" value="adopt" disabled={busy}>
            ポリシーを保存
          </Button>
        </Form>
      </section>
    </div>
  );
}
