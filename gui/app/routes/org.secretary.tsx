import { data, isRouteErrorResponse, redirect } from "react-router";
import { getCelerisClient } from "~/celeris/client.server";
import { loadConversation, runConversationAction } from "~/celeris/conversation.server";
import { type CelerisRouteErrorData, celerisErrorResponse, isCelerisUnavailable } from "~/celeris/errors";
import { Conversation } from "~/components/Conversation";
import { type ConversationData, SECRETARY_NODE_ID } from "~/lib/conversation";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { CelerisBanner } from "~/root";
import type { Route } from "./+types/org.secretary";

/**
 * `/org/secretary`（秘書との対話。SPEC §3.1・§4 の 1「案件を投げる、状況を聞く、方針を変える」、
 * ADR-0033 D4）。`/org/:id` と同じ中継・同じ部品で、相手が `secretary` に固定されている点だけが違う
 * （ナビの「秘書」がここを指す。`/org/:id` より静的なルートが優先される）。
 *
 * `/` はここへ 302 する最初の画面（`routes/home.tsx`、Phase G13f-1）なので、celeris 停止中も 200 で開く契約
 * （docs/DESIGN.md §10 Phase G0 受け入れ条件 4）はここが引き継ぐ（Phase G13g）。root が celeris の状態を
 * 独自に検査してバナーを出す（`app/root.tsx`）ので、ここでは celeris に届かないときだけ捕まえて「対話は
 * まだ何も無い」状態として描く（他の画面が `CelerisUnavailable` を投げて 5xx にする作法とは分ける。
 * `routes/inbox.tsx` の `loadInbox` と同じ考え方）。
 */

export async function loader({ request }: Route.LoaderArgs): Promise<ConversationData> {
  try {
    return await loadConversation(getCelerisClient(), SECRETARY_NODE_ID, request);
  } catch (e) {
    if (isCelerisUnavailable(e)) {
      return {
        nodeId: SECRETARY_NODE_ID,
        node: null,
        projects: [],
        projectId: null,
        messages: [],
        attention: [],
        clusters: [],
      };
    }
    throw celerisErrorResponse(e);
  }
}

export const shouldRevalidate = revalidateAfterActionErrors;

export function meta(_: Route.MetaArgs) {
  return [{ title: "秘書 - Celeris" }];
}

/**
 * 話しかける。案件を選ばず「新しい案件として」で送ると `POST /projects`（SPEC §4 の 1「案件を投げる」を
 * 対話に統合したもの）。作った直後に秘書が最初の返事（理解確認・方針・最初の途中目標）を返す（SPEC §7）ので、
 * その案件を選んだ状態（`?waiting=1`）へ移って返事を待つ。
 */
export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const outcome = await runConversationAction(getCelerisClient(), SECRETARY_NODE_ID, form, request.signal);
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
    const errorData = error.data as CelerisRouteErrorData;
    if (errorData.kind === "unavailable") {
      return (
        <main className="p-4">
          <CelerisBanner celerisApiUrl={errorData.baseUrl ?? ""} problem={null} />
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
            ? "組織にまだ秘書（id: secretary）がいません。「組織」の画面で秘書を作るか、celeris の org_include（config/org.example.toml）で種蒔きしてください。"
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
