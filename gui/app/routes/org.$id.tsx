import { data, isRouteErrorResponse, redirect } from "react-router";
import { Conversation } from "~/components/Conversation";
import type { ConversationData } from "~/lib/conversation";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { TaskdBanner } from "~/root";
import { getTaskdClient } from "~/taskd/client.server";
import { loadConversation, runConversationAction } from "~/taskd/conversation.server";
import { isTaskdUnavailable, type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import type { Route } from "./+types/org.$id";

/**
 * `/org/:id`（組織の木から選んだ「人」との対話。SPEC §3.4、ADR-0033 D4、
 * docs/taskd-api-v1.md §3.54〜3.55）。秘書は `/org/secretary`（同じ部品・同じ中継を使う別ルート、
 * Phase G13g で taskd 停止中も 200 にした）。ここも同じ作法にそろえる: taskd に届かないときは
 * 捕まえて「対話はまだ何も無い」状態として描く（root が独自にバナーを出す）。
 */

export async function loader({ params, request }: Route.LoaderArgs): Promise<ConversationData> {
  try {
    return await loadConversation(getTaskdClient(), params.id, request);
  } catch (e) {
    if (isTaskdUnavailable(e)) {
      return {
        nodeId: params.id,
        node: null,
        projects: [],
        projectId: null,
        messages: [],
        attention: [],
        clusters: [],
      };
    }
    throw taskdErrorResponse(e);
  }
}

export const shouldRevalidate = revalidateAfterActionErrors;

export function meta({ params }: Route.MetaArgs) {
  return [{ title: `${params.id} と話す - Celeris` }];
}

/**
 * 話しかける（`POST /org/{id}/messages` → 202 をそのまま返す）。秘書に「新しい案件として」送ったときだけ
 * `POST /projects` になり、作った案件を選んだ状態（`?waiting=1`: 秘書の最初の返事を待つ）へ移る。
 */
export async function action({ params, request }: Route.ActionArgs) {
  const form = await request.formData();
  const outcome = await runConversationAction(getTaskdClient(), params.id, form, request.signal);
  if (outcome.ok && outcome.op === "new_project") {
    return redirect(
      `/org/${encodeURIComponent(params.id)}?project=${encodeURIComponent(outcome.project.id)}&waiting=1`,
    );
  }
  return data(outcome, { status: outcome.ok ? 202 : outcome.error.status });
}

export default function OrgConversationPage({ loaderData }: Route.ComponentProps) {
  return <Conversation data={loaderData} />;
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  if (isRouteErrorResponse(error) && error.data && typeof error.data === "object" && "kind" in error.data) {
    const errorData = error.data as TaskdRouteErrorData;
    if (errorData.kind === "unavailable") {
      return (
        <main className="p-4">
          <TaskdBanner taskdApiUrl={errorData.baseUrl ?? ""} problem={null} />
        </main>
      );
    }
    return (
      <main className="p-4">
        <h1 className="text-xl font-semibold">
          {errorData.status === 404 ? "その人は組織にいません" : `エラー ${errorData.status}`}
        </h1>
        <p className="mt-2 text-sm text-fg-muted">{errorData.detail}</p>
      </main>
    );
  }
  return (
    <main className="p-4">
      <h1 className="text-xl font-semibold">エラー</h1>
      <p className="mt-2 text-sm text-fg-muted">予期しないエラーが起きました。</p>
    </main>
  );
}
