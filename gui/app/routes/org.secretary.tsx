import { data, isRouteErrorResponse, redirect } from "react-router";
import { Conversation } from "~/components/Conversation";
import { type ConversationData, SECRETARY_NODE_ID } from "~/lib/conversation";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { TaskdBanner } from "~/root";
import { getTaskdClient } from "~/taskd/client.server";
import { loadConversation, runConversationAction } from "~/taskd/conversation.server";
import { type TaskdRouteErrorData, taskdErrorResponse } from "~/taskd/errors";
import type { Route } from "./+types/org.secretary";

/**
 * `/org/secretary`（秘書との対話。SPEC §3.1・§4 の 1「案件を投げる、状況を聞く、方針を変える」、
 * ADR-0033 D4）。`/org/:id` と同じ中継・同じ部品で、相手が `secretary` に固定されている点だけが違う
 * （ナビの「秘書」がここを指す。`/org/:id` より静的なルートが優先される）。
 */

export async function loader({ request }: Route.LoaderArgs): Promise<ConversationData> {
  try {
    return await loadConversation(getTaskdClient(), SECRETARY_NODE_ID, request);
  } catch (e) {
    throw taskdErrorResponse(e);
  }
}

export const shouldRevalidate = revalidateAfterActionErrors;

export function meta(_: Route.MetaArgs) {
  return [{ title: "秘書 - taskd-gui" }];
}

/**
 * 話しかける。案件を選ばず「新しい案件として」で送ると `POST /projects`（SPEC §4 の 1「案件を投げる」を
 * 対話に統合したもの）。作った直後に秘書が最初の返事（理解確認・方針・最初の途中目標）を返す（SPEC §7）ので、
 * その案件を選んだ状態（`?waiting=1`）へ移って返事を待つ。
 */
export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const outcome = await runConversationAction(getTaskdClient(), SECRETARY_NODE_ID, form, request.signal);
  if (outcome.ok && outcome.op === "new_project") {
    return redirect(`/org/secretary?project=${encodeURIComponent(outcome.project.id)}&waiting=1`);
  }
  return data(outcome, { status: outcome.ok ? 202 : outcome.error.status });
}

export default function SecretaryPage({ loaderData }: Route.ComponentProps) {
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
          {errorData.status === 404 ? "秘書がいません" : `エラー ${errorData.status}`}
        </h1>
        <p className="mt-2 text-sm text-fg-muted">
          {errorData.status === 404
            ? "組織にまだ秘書（id: secretary）がいません。「組織」の画面で秘書を作るか、taskd の org_include（config/org.example.toml）で種蒔きしてください。"
            : errorData.detail}
        </p>
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
