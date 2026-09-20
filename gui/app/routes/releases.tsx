import { useEffect, useId, useState } from "react";
import { data, type FetcherWithComponents, isRouteErrorResponse, useFetcher, useRevalidator } from "react-router";
import type { ReleasePromoteOutcome } from "~/celeris/action-types";
import { type CelerisClient, getCelerisClient } from "~/celeris/client.server";
import { type CelerisRouteErrorData, celerisErrorResponse } from "~/celeris/errors";
import { promoteRelease, readReleaseSha12 } from "~/celeris/releases-admin.server";
import type { ReleaseItem, Releases } from "~/celeris/types";
import { ReleasePromoteFlash } from "~/components/Flash";
import { HelpLink } from "~/components/HelpLink";
import { Badge } from "~/components/ui/badge";
import { Button } from "~/components/ui/button";
import { Card, CardBody, CardHeader } from "~/components/ui/card";
import { hintClass, inputClass, labelClass } from "~/components/ui/form";
import { Icon } from "~/components/ui/Icon";
import { Alert, DataItem, EmptyState, Mono, PageHeader, SectionTitle } from "~/components/ui/misc";
import { instanceRoleLabel } from "~/lib/labels";
import {
  changesSummaryText,
  commitShort,
  handoffInFlight,
  handoffProgressText,
  notOnMainText,
  promoteAvailability,
  promoteConfirmText,
  promotedAtText,
  promoteNeedsTypedSha,
  releaseGateLabel,
  releasePositionLabel,
  releaseSubtitle,
  releaseVerifyLabel,
  releaseVerifyTone,
  sensitiveBadgeText,
  staleChangesText,
  typedShaMatches,
} from "~/lib/releases";
import { revalidateAfterActionErrors } from "~/lib/revalidate";
import { CelerisBanner } from "~/root";
import type { Route } from "./+types/releases";

/**
 * `/releases`（リリース画面、Phase G14。ADR-0040 D6、docs/celeris-api-v1.md §3.66〜3.67）の
 * loader が返すデータ。
 *
 * `GET /releases` の応答をそのまま渡す（並び・`is_current` / `promoting` は celeris が計算済みなので、
 * GUI 側で再計算しない）。表示の判断は `~/lib/releases.ts` の純粋関数に寄せてある。
 */
export interface ReleasesData {
  releases: Releases;
  fetchedAt: string;
}

/** `GET /releases` を呼ぶ。応答はそのまま返す（派生の集計はしない）。 */
export async function loadReleases(client: CelerisClient, request: Request): Promise<ReleasesData> {
  const releases = await client.get<Releases>("/releases", { signal: request.signal });
  return { releases, fetchedAt: new Date().toISOString() };
}

// 404 / 409 の action 後も再検証する（docs/adr/0005 D2）。`fetcher.data` は再検証では消えないので、
// 409「未検証です」等の結果は行に出たまま残る（監査 H1）。
export const shouldRevalidate = revalidateAfterActionErrors;

export async function loader({ request }: Route.LoaderArgs): Promise<ReleasesData> {
  try {
    return await loadReleases(getCelerisClient(), request);
  } catch (e) {
    throw celerisErrorResponse(e);
  }
}

export function meta(_: Route.MetaArgs) {
  return [{ title: "リリース - Celeris" }];
}

/**
 * 昇格（ADR-0040 D6）。GUI 側に判断は無く、フォームの `sha12` を `POST /releases/{sha12}/promote` に
 * 写すだけ。**押すのは人**（D5）。**`POST /reload` は呼ばない**（設定は変わらない）。
 */
export async function action({ request }: Route.ActionArgs) {
  const form = await request.formData();
  const intent = form.get("intent");
  if (intent !== "release_promote") {
    throw data({ error: `unknown intent: ${String(intent)}` }, { status: 400 });
  }
  const outcome = await promoteRelease(getCelerisClient(), readReleaseSha12(form), request.signal);
  return data(outcome, { status: outcome.ok ? 202 : outcome.error.status });
}

/** 引き継ぎ中に `GET /releases` を読み直す間隔（ADR-0040 D4。SSE には載らないのでここだけポーリング）。 */
export const HANDOFF_POLL_MS = 2_000;

export default function ReleasesPage({ loaderData }: Route.ComponentProps) {
  const { releases } = loaderData;
  const revalidator = useRevalidator();
  const inFlight = handoffInFlight(releases);
  const progress = handoffProgressText(releases);

  // 昇格の最中だけ 2 秒ごとに読み直す（`/root.tsx` の再接続ポーリングと同じ作り）。
  // 引き継ぎは SSE のイベントにならない（タスクのイベントではない）ので、ここだけは自前で回す。
  useEffect(() => {
    if (!inFlight) return;
    const id = setInterval(() => {
      if (revalidator.state === "idle") revalidator.revalidate();
    }, HANDOFF_POLL_MS);
    return () => clearInterval(id);
  }, [inFlight, revalidator]);

  return (
    <div className="space-y-8">
      <PageHeader
        icon="layers"
        title={
          <>
            リリース
            <HelpLink anchor="screens" label="画面ごとの説明" />
          </>
        }
        description="ビルド済みのリリースの検証状態を見て、検証済みのものへ昇格します（昇格は人が押します）。"
      />

      <section aria-labelledby="running-heading" data-testid="releases-running" className="space-y-4">
        <SectionTitle icon="activity" id="running-heading">
          いま動いているもの
        </SectionTitle>
        <Card>
          <CardBody>
            <dl className="grid grid-cols-2 gap-x-4 gap-y-3 text-sm sm:grid-cols-4">
              <DataItem label="リリース">
                <Mono className="text-sm text-fg" data-testid="running-release">
                  {releases.running.release}
                </Mono>
              </DataItem>
              <DataItem label="役割">
                <span data-testid="running-role">{instanceRoleLabel(releases.running.role)}</span>
              </DataItem>
              <DataItem label="現行（current）">
                <Mono className="text-sm text-fg" data-testid="current-release">
                  {releases.current ?? "-"}
                </Mono>
              </DataItem>
              <DataItem label="直前（previous）">
                <Mono className="text-sm text-fg" data-testid="previous-release">
                  {releases.previous ?? "-"}
                </Mono>
              </DataItem>
            </dl>
          </CardBody>
        </Card>
      </section>

      <section aria-labelledby="handoff-heading" data-testid="releases-handoff" className="space-y-4">
        <SectionTitle icon="refresh" id="handoff-heading" count={releases.instances.length}>
          切り替えの進行
        </SectionTitle>
        {progress ? (
          <Alert tone="warning" title="切り替え中です" data-testid="handoff-progress">
            <p>{progress}</p>
            <p className={hintClass}>
              旧いプロセスは手元の仕事を最後まで見てから終わります（最長 1 時間）。この画面は 2 秒ごとに
              自動で読み直しています。
            </p>
          </Alert>
        ) : (
          <p className="text-sm text-fg-muted" data-testid="handoff-idle">
            切り替えは走っていません。
          </p>
        )}
        {releases.instances.length > 0 && (
          <ul className="space-y-1 text-sm" data-testid="instance-list">
            {releases.instances.map((instance) => (
              <li
                key={instance.instance_id}
                data-testid="instance-row"
                data-instance-role={instance.role}
                className="flex flex-wrap items-center gap-2"
              >
                <Mono className="text-sm text-fg">{instance.release}</Mono>
                <Badge tone={instance.role === "active" ? "success" : "warning"}>
                  {instanceRoleLabel(instance.role)}
                </Badge>
                <span className="text-fg-subtle">pid {instance.pid}</span>
                <span className="text-fg-subtle">{instance.started_at}</span>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section aria-labelledby="releases-heading" data-testid="releases-section" className="space-y-4">
        <SectionTitle icon="layers" id="releases-heading" count={releases.items.length}>
          リリース一覧
        </SectionTitle>
        {releases.items.length === 0 ? (
          <EmptyState icon="layers" title="リリースがありません">
            `scripts/selfdeploy/release.sh &lt;ref&gt;` を通すと、ここに並びます。
          </EmptyState>
        ) : (
          <div className="grid gap-4 xl:grid-cols-2">
            {releases.items.map((item) => (
              <ReleaseCard key={item.sha12} item={item} />
            ))}
          </div>
        )}
      </section>
    </div>
  );
}

/**
 * 1 リリースのカード。**行ごとに 1 つの fetcher**（key = sha12）を持たせて、202 / 409 の結果が
 * その行に残るようにする（SSE の再検証では `fetcher.data` は消えない。監査 H1）。
 */
function ReleaseCard({ item }: { item: ReleaseItem }) {
  const fetcher: FetcherWithComponents<ReleasePromoteOutcome> = useFetcher<ReleasePromoteOutcome>({
    key: `release-${item.sha12}`,
  });
  const submitting = fetcher.state !== "idle";
  const { canPromote, reason } = promoteAvailability(item);
  const position = releasePositionLabel(item);
  const sensitive = sensitiveBadgeText(item);
  const notOnMain = notOnMainText(item);
  const promotedAt = promotedAtText(item);
  const summary = changesSummaryText(item);
  const stale = staleChangesText(item);
  // 安全に関わる変更があるときは sha12 を打たせる（ADR-0041 D4）。打った文字はこの行だけの状態。
  const needsTyped = promoteNeedsTypedSha(item);
  const [typed, setTyped] = useState("");
  const typedOk = typedShaMatches(item, typed);
  const shaInputId = useId();

  return (
    <Card
      id={`release-${item.sha12}`}
      data-testid="release-row"
      data-release-sha12={item.sha12}
      className="hover:shadow-md"
    >
      <CardHeader
        icon="layers"
        tone={releaseVerifyTone(item)}
        title={
          <Mono className="text-sm font-semibold text-fg" data-testid="release-sha12">
            {item.sha12}
          </Mono>
        }
        description={
          <span data-testid="release-subtitle" className="break-all">
            {releaseSubtitle(item)}
          </span>
        }
        actions={
          <>
            {position && (
              <Badge tone="primary" data-testid="release-position">
                {position}
              </Badge>
            )}
            <Badge tone={item.gate_ok ? "success" : "danger"} data-testid="release-gate">
              {releaseGateLabel(item)}
            </Badge>
            <Badge tone={releaseVerifyTone(item)} dot pulse={item.promoting} data-testid="release-verify">
              {releaseVerifyLabel(item)}
            </Badge>
            {sensitive && (
              <Badge tone="danger" dot data-testid="release-sensitive-badge">
                {sensitive}
              </Badge>
            )}
          </>
        }
      />
      <CardBody className="space-y-4">
        <dl className="grid grid-cols-2 gap-x-4 gap-y-3 text-sm sm:grid-cols-3">
          <DataItem label="ビルド">
            <span data-testid="release-built-at" className="text-fg-subtle">
              {item.built_at ?? "-"}
            </span>
          </DataItem>
          <DataItem label="ref">
            <span data-testid="release-ref" className="break-all">
              {item.ref ?? "-"}
            </span>
          </DataItem>
          <DataItem label="schema_version">
            <span data-testid="release-schema-version">{item.schema_version ?? "-"}</span>
          </DataItem>
          <DataItem label="検証">
            <span data-testid="release-verify-at" className="text-fg-subtle">
              {item.verify?.at ?? "-"}
            </span>
          </DataItem>
          <DataItem label="昇格">
            <span data-testid="release-promoted-at" className="text-fg-subtle">
              {promotedAt ?? "まだ"}
            </span>
          </DataItem>
        </dl>

        {notOnMain && (
          <Alert tone="warning" title="main に戻っていません" data-testid="release-not-on-main">
            <p className="break-all">
              <Mono className="text-sm">{notOnMain}</Mono>
            </p>
            <p className={hintClass}>
              昇格は本番を動かすだけで、あなたのチェックアウトには触れません（ADR-0041 D3）。 上のコマンドを人が流すと
              `main` が本番に追いつきます。
            </p>
          </Alert>
        )}

        {item.changes && (
          <details className="rounded-lg border border-border bg-surface-2/40" data-testid="release-changes">
            <summary className="cursor-pointer list-none px-3 py-2 text-sm text-fg-muted hover:text-fg">
              <Icon name="layers" className="mr-1.5 inline size-4" />
              昇格したら変わるもの
              <span className="ml-2 text-fg-subtle" data-testid="release-changes-summary">
                {summary}
              </span>
            </summary>
            <div className="space-y-3 px-3 pb-3">
              {stale && (
                <p className={hintClass} data-testid="release-changes-stale">
                  {stale}
                </p>
              )}
              {item.changes.commits.length > 0 ? (
                <ul className="space-y-1 text-sm" data-testid="release-commit-list">
                  {item.changes.commits.map((commit) => (
                    <li key={commit.sha} data-testid="release-commit" className="flex gap-2">
                      <Mono className="shrink-0 text-xs text-fg-subtle">{commitShort(commit)}</Mono>
                      <span className="break-all">{commit.subject}</span>
                    </li>
                  ))}
                </ul>
              ) : (
                <p className={hintClass} data-testid="release-commit-empty">
                  コミットの一覧がありません（起点が分からないか、差が無いリリースです）。
                </p>
              )}
              <p className={hintClass} data-testid="release-file-count">
                変更ファイル {item.changes.file_count} 件
              </p>
            </div>
          </details>
        )}

        {sensitive && item.changes && (
          <Alert tone="danger" title={sensitive} data-testid="release-sensitive">
            <p>
              昇格の仕組み・本番の設定・エージェントへの指示文に当たるファイルが変わっています。
              中身を読んでから押してください。
            </p>
            <ul className="mt-2 space-y-0.5" data-testid="release-sensitive-list">
              {item.changes.sensitive.map((path) => (
                <li key={path} data-testid="release-sensitive-path">
                  <Mono className="text-xs break-all">{path}</Mono>
                </li>
              ))}
            </ul>
          </Alert>
        )}

        {item.problem && (
          <Alert tone="danger" title="リリースのファイルが読めません" data-testid="release-problem">
            <p className="break-all">{item.problem}</p>
          </Alert>
        )}

        {item.promoting && (
          <Alert tone="warning" title="昇格が走っています" data-testid="release-promoting">
            <p>このリリースへの切り替えが進行中です。完了まで数十秒かかります。</p>
          </Alert>
        )}

        <ReleasePromoteFlash outcome={fetcher.data} />

        {canPromote ? (
          <details className="group">
            <summary className="inline-flex h-8 cursor-pointer list-none items-center gap-1.5 rounded-lg border border-primary-border bg-primary-soft px-3 text-sm text-primary-soft-fg shadow-xs hover:bg-primary hover:text-white">
              <Icon name="rotate" className="size-4" />
              昇格
            </summary>
            <fetcher.Form method="post" className="mt-2 rounded-lg border border-primary-border bg-primary-soft/40 p-3">
              <input type="hidden" name="intent" value="release_promote" />
              <input type="hidden" name="sha12" value={item.sha12} />
              <p className="mb-2 text-sm text-fg-muted" data-testid="release-promote-confirm">
                {promoteConfirmText(item)}
              </p>
              {needsTyped && (
                <div className="mb-2 space-y-1" data-testid="release-promote-typed">
                  <label className={labelClass} htmlFor={shaInputId}>
                    続けるには <Mono className="text-sm">{item.sha12}</Mono> を入力してください
                  </label>
                  <input
                    id={shaInputId}
                    type="text"
                    className={`${inputClass} font-mono`}
                    autoComplete="off"
                    spellCheck={false}
                    value={typed}
                    onChange={(e) => setTyped(e.target.value)}
                    data-testid="release-promote-sha-input"
                  />
                  <p className={hintClass}>
                    安全に関わる変更を含むリリースは、ボタンを押すだけでは昇格できません（ADR-0041 D4）。
                  </p>
                </div>
              )}
              <Button
                type="submit"
                variant="primary"
                size="sm"
                disabled={submitting || (needsTyped && !typedOk)}
                data-testid="release-promote"
                onClick={(e) => {
                  // 安全に関わる変更があるときは、上の sha12 入力がそのまま確認になる（`confirm` は聞かない）。
                  if (needsTyped) {
                    if (!typedOk) e.preventDefault();
                    return;
                  }
                  // 二重の確認（ADR-0040 D6「確認付き」）。ブラウザ以外（テスト・SSR）では confirm が
                  // 無いので、あるときだけ聞く。
                  if (typeof window !== "undefined" && typeof window.confirm === "function") {
                    if (!window.confirm(promoteConfirmText(item))) e.preventDefault();
                  }
                }}
              >
                <Icon name="rotate" />
                昇格する
              </Button>
            </fetcher.Form>
          </details>
        ) : (
          <p className="text-sm text-fg-muted" data-testid="release-promote-disabled">
            昇格できません: {reason}
          </p>
        )}
      </CardBody>
    </Card>
  );
}

/**
 * loader が `celerisErrorResponse` で投げた `Response` を判別する（`app/routes/clusters.tsx` と同じ方針）。
 * celeris 停止中はバナー、それ以外は status と detail を出す。
 */
export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  if (isRouteErrorResponse(error) && error.data && typeof error.data === "object" && "kind" in error.data) {
    const data = error.data as CelerisRouteErrorData;
    if (data.kind === "unavailable") {
      return (
        <main className="p-4">
          <CelerisBanner celerisApiUrl={data.baseUrl ?? ""} problem={null} />
        </main>
      );
    }
    return (
      <main className="p-4">
        <h1 className="text-xl font-semibold">エラー {data.status}</h1>
        <p className="mt-2 text-sm text-fg-muted">{data.detail}</p>
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
