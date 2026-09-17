import type { ReactNode } from "react";
import { Link } from "react-router";
import { StatusBadge } from "~/components/ui/badge";
import { Card, CardBody } from "~/components/ui/card";
import { tableClass, tdClass, thClass, theadClass, trHoverClass } from "~/components/ui/form";
import { Icon, type IconName } from "~/components/ui/Icon";
import { PageHeader } from "~/components/ui/misc";
import type { Tone } from "~/components/ui/tone";
import { TONE_ICON_WRAP } from "~/components/ui/tone";
import { cn } from "~/lib/utils";
import type { Route } from "./+types/help";

/**
 * `/help`（使い方ページ、docs/DESIGN.md §10 Phase G6）。
 * taskd に問い合わせない静的なページ（loader 無し）。内容は `docs/taskd-api-v1.md` と
 * `docs/DESIGN.md` の範囲だけに留める（仕様に無い機能は書かない）。
 */
export function meta(_: Route.MetaArgs) {
  return [{ title: "使い方 - taskd-gui" }];
}

const STATUS_ROWS: { status: string; meaning: string; canDo: string }[] = [
  {
    status: "draft",
    meaning: "作成直後、まだ受け入れられていない（execute/plan）。",
    canDo: "承認（受け入れ）／取り消し",
  },
  {
    status: "ready",
    meaning: "実行待ち（taskd が拾う）。kind=approval の ready は「人間の承認待ち」。",
    canDo: "kind=approval なら承認／却下、それ以外は取り消しのみ（実行開始は taskd が行う）",
  },
  { status: "running", meaning: "ワーカーが実行中。", canDo: "取り消しのみ（待つ）" },
  {
    status: "blocked",
    meaning:
      "人間への質問を残して止まっている（ワーカーが聞いた場合と、委譲した子が失敗して親がやり直せなかった場合がある）。",
    canDo: "回答／取り消し",
  },
  {
    status: "reviewing",
    meaning: "受け入れ条件を taskd（またはレビュー run）が判定中。",
    canDo: "取り消しのみ（待つ）",
  },
  { status: "done", meaning: "受け入れ条件を全て満たして完了。", canDo: "（終端。操作なし）" },
  { status: "failed", meaning: "リトライ上限に達した、または致命的なエラー。", canDo: "（終端。操作なし）" },
  {
    status: "cancelled",
    meaning: "取り消された（自分で取り消した、または依存先が失敗して連鎖）。",
    canDo: "（終端。操作なし）",
  },
];

const GLOSSARY: { term: string; text: string }[] = [
  {
    term: "タスク",
    text: "taskd が管理する作業単位。kind は execute / plan / approval / review、status で進行状況を表す。",
  },
  {
    term: "run",
    text: "タスクの 1 回のワーカー実行。requeue のたびに新しい run が始まる（同じタスクに複数の run がありうる）。",
  },
  {
    term: "プロバイダ（アカウント）",
    text: "ワーカーを起動する AI プロバイダの実行アカウント設定（tier・同時実行数・モデル）。",
  },
  { term: "リース", text: "running 中のタスクに taskd が与える実行権限の期限。切れると requeue に回る。" },
  { term: "requeue", text: "run が失敗する・リースが切れるなどでタスクが再び実行待ちに戻ること。" },
  {
    term: "cooldown",
    text: "プロバイダ（またはアカウント）がスロットル等で一時的に使えない期間。解けると自動で再び使われる。",
  },
  {
    term: "account_pool",
    text: "claude-code か codex のプロバイダが、単一の env ではなく [accounts]（claude_dir / codex_dir）のそのアダプタのアカウントのプールから残量で選んで実行する設定。",
  },
  {
    term: "API キー",
    text: "検索エンジン等（例: web-research 分野が使う Tavily・Exa の検索 API キー）をワーカーの環境変数に流し込むために taskd が預かる秘密。[secrets] dir 配下に 1 秘密 1 ファイル（0600）で保存され、値は保存後 GUI にも API 応答にも二度と表示されない。/accounts の「API キー」節から追加・更新・削除でき、保存・削除のたびに reload が走って設定（env_from_secrets）に反映される。",
  },
  {
    term: "Plan",
    text: "複数の子タスクをまとめる親タスク（kind=plan）。子タスクは draft で作られ、人間が受け入れて進める。",
  },
  { term: "Approval", text: "人間の承認を待つための子タスク（kind=approval）。承認／却下で親の判定が決まる。" },
  {
    term: "成果物",
    text: "ワーカーが taskd に明示的に登録したファイル（ディスクを自動スキャンして拾うことはしない）。",
  },
  {
    term: "分野（genre）",
    text: "タスクが属する専門領域（例: コーディング、関連研究調査）。設定の [[genres]] にある id で、どのハーネス・役割の集団に作業を投げるかの入口になる（実際にどのアダプタで動くかは、その分野の既定役割などが持つ）。委譲できる親タスクには使える分野と役割の一覧が渡され、子タスクを別の分野に委譲できる。分野は「できること」（capabilities）と「渡すもの／返るもの」（input_artifacts / output_artifacts）も広告でき、Plannerや委譲する親タスクはどこに作業を送るか選ぶときにその一覧を見る。",
  },
  {
    term: "接続方式（auth）",
    text: "クラスタ（[[clusters]]）ごとに設定する、接続の張り方。manual（既定）は taskd が自分では接続を張らず、人が手元で scripts/cluster-login.sh を実行する。publickey は鍵だけで入れるクラスタで、クラスタ画面の「接続」ボタンを押すだけで taskd が張る（ディスパッチャが自動でも試みる）。totp は publickey の後に検証コード（2 要素認証）が要るクラスタで、「接続」→ 表示されたプロンプトを見て検証コードを入力 →「送信」。コードはその場で ssh に渡すだけで taskd には保存されず、ログにも画面にも残らない。",
  },
];

const TOC = [
  { id: "flow", heading: "3 分で分かる流れ", icon: "play" },
  { id: "screens", heading: "画面ごとの説明", icon: "layers" },
  { id: "acceptance", heading: "受け入れ条件", icon: "checkCircle" },
  { id: "status", heading: "状態", icon: "activity" },
  { id: "glossary", heading: "用語集", icon: "book" },
  { id: "trouble", heading: "困ったとき", icon: "help" },
] satisfies { id: string; heading: string; icon: IconName }[];

const SCREENS: { href: string | null; icon: IconName; title: string; text: string }[] = [
  {
    href: "/",
    icon: "inbox",
    title: "受信箱",
    text: "人間の対応が要る項目（承認待ち・質問・受け入れ待ちの draft・注意）だけを集めた画面。まずここを開く。",
  },
  {
    href: "/tasks",
    icon: "list",
    title: "一覧",
    text: "全タスクを状態・条件で絞り込んで見る画面。特定のタスクを探すときに開く。",
  },
  {
    href: null,
    icon: "file",
    title: "詳細（/tasks/<id>）",
    text: "1 件のタスクの受け入れ条件・run・イベントの流れ・成果物を見る画面。一覧や受信箱から個々のタスクを開くと表示される。",
  },
  {
    href: "/graph",
    icon: "network",
    title: "DAG",
    text: "タスク同士の依存関係と Plan の親子関係を図で見る画面。全体の進み具合を俯瞰したいときに開く。",
  },
  {
    href: "/providers",
    icon: "cpu",
    title: "プロバイダ",
    text: "各アカウントの利用状況（done / requeue の件数、トークン、cooldown）を見る画面。追加・編集・削除・疎通確認もここで行う（管理系 API のトークンが要る）。動きが遅い・偏っていると感じたら開く。",
  },
  {
    href: "/accounts",
    icon: "users",
    title: "アカウント",
    text: "account_pool = true のプロバイダが使う claude-code（[accounts] claude_dir）/ codex（[accounts] codex_dir）のアカウントのプール（ログイン状態・5 時間枠/週次枠の使用率・score）を見る画面。追加・ログイン・残量確認・削除もここで行う（管理系 API のトークンが要る）。ログインの流儀はアダプタで違う: claude-code は URL を開いて表示されたコードをこの画面に貼り戻す。codex は `codex login --device-auth` を中継し、URL と一回限りのコード（user_code）を表示するだけで、コードはこの画面には貼り戻さない（別のデバイスでその URL を開いて入力する）。ログインが終わるとこの画面が自動で更新される。下部の「API キー」節では、検索 API キー等の秘密（[secrets] dir）の追加・更新・削除ができる（管理系 API のトークンが要る。値は保存後二度と表示されない）。",
  },
  {
    href: "/clusters",
    icon: "server",
    title: "クラスタ",
    text: "リモートで実行するタスクが使う `[[clusters]]` の接続状況（connected・cooldown・auth）を見る画面。受信箱の「クラスタに接続できません」から開くことが多い。接続方式（auth）が manual 以外なら、この画面から接続もできる（管理系 API のトークンが要る）: publickey は「接続」ボタンだけ、totp は「接続」→ プロンプト表示 → 検証コード入力 → 「送信」（コードはログにも応答にも残らない）。manual は従来どおり手元で scripts/cluster-login.sh を実行する。",
  },
  {
    href: "/daemon",
    icon: "activity",
    title: "デーモン",
    text: "taskd 本体の状態（pid・tick・実行中の run・承認待ち・経路なしのタスク）と replay を見る画面。taskd 自体の様子を確認したいときに開く。",
  },
];

export default function HelpPage() {
  return (
    <div className="max-w-3xl space-y-8">
      <PageHeader
        as="h1"
        icon="book"
        title="使い方"
        description="taskd-gui の使い方をひとまとめにしたドキュメントです。"
      />

      <nav aria-label="使い方の目次" className="rounded-xl border border-border bg-surface-2/50 p-3">
        <ul className="flex flex-wrap gap-1.5 text-sm">
          {TOC.map((item) => (
            <li key={item.id}>
              <a
                href={`#${item.id}`}
                className="inline-flex items-center gap-1.5 rounded-lg border border-border bg-surface px-2.5 py-1.5 font-medium text-fg-muted shadow-xs transition-colors hover:border-primary-border hover:bg-primary-soft hover:text-primary-soft-fg"
              >
                <Icon name={item.icon} className="size-3.5 text-fg-subtle" />
                {item.heading}
              </a>
            </li>
          ))}
        </ul>
      </nav>

      <Section id="flow" icon="play" tone="primary" heading="3 分で分かる流れ" testId="help-flow-section">
        <ol className="list-decimal space-y-1.5 pl-5 text-sm text-fg">
          <li>
            <Link
              to="/tasks/new"
              className="text-primary underline decoration-primary/40 underline-offset-2 hover:decoration-primary"
            >
              タスクを作る
            </Link>
            （または{" "}
            <Link
              to="/plans/new"
              className="text-primary underline decoration-primary/40 underline-offset-2 hover:decoration-primary"
            >
              Plan を作る
            </Link>
            ）
          </li>
          <li>人間が承認する（draft を受け入れる。kind=approval のタスクは承認／却下で判定する）</li>
          <li>taskd がワーカーを起動する（人間は何もしない。順番・タイミングは taskd が決める）</li>
          <li>taskd が受け入れ条件を自分で判定する（ワーカーの自己申告は信じない。再実行して確かめる）</li>
          <li>条件を満たせば done。満たせなければ requeue して再試行するか failed になる</li>
        </ol>
        <p className="mt-3 text-sm text-fg-muted">
          人間が触るのは<strong className="font-semibold text-fg">承認・回答・取り消し</strong>だけ。何をいつ動かすかは
          taskd が決める。
        </p>
      </Section>

      <Section id="screens" icon="layers" tone="teal" heading="画面ごとの説明" testId="help-screens-section">
        <dl className="divide-y divide-border">
          {SCREENS.map((s) => (
            <div key={s.title} className="py-3 first:pt-0 last:pb-0">
              <dt className="flex items-center gap-3 font-semibold text-fg">
                <span className="grid size-8 shrink-0 place-items-center rounded-lg bg-surface-2 text-fg-subtle ring-1 ring-border">
                  <Icon name={s.icon} className="size-4" />
                </span>
                {s.href ? (
                  <Link to={s.href} className="hover:underline">
                    {s.title}
                  </Link>
                ) : (
                  s.title
                )}
              </dt>
              <dd className="mt-0.5 pl-11 text-sm text-fg-muted">{s.text}</dd>
            </div>
          ))}
        </dl>
      </Section>

      <Section
        id="acceptance"
        icon="checkCircle"
        tone="success"
        heading="受け入れ条件"
        testId="help-acceptance-section"
      >
        <p className="text-sm text-fg-muted">
          タスクの受け入れ条件（acceptance）は 4 種類。
          <strong className="font-semibold text-fg">判定は taskd が自分で再実行して確かめる</strong>
          （ワーカーが「テストを通した」と言っても、それだけでは信じない）。
        </p>
        <dl className="mt-3 space-y-3 text-sm">
          <div>
            <dt>
              <code className="rounded bg-surface-2 px-1.5 py-0.5 font-mono text-xs font-semibold text-fg">
                command
              </code>
            </dt>
            <dd className="mt-1 text-fg-muted">
              シェルコマンドを実行し、終了コードが期待値（既定 0）と一致すれば通る。 例:{" "}
              <code className="rounded bg-surface-2 px-1 py-0.5 font-mono text-xs">
                {'{"type":"command","cmd":"cargo test","expect_exit":0}'}
              </code>
            </dd>
          </div>
          <div>
            <dt>
              <code className="rounded bg-surface-2 px-1.5 py-0.5 font-mono text-xs font-semibold text-fg">
                artifact_exists
              </code>
            </dt>
            <dd className="mt-1 text-fg-muted">
              指定した名前の成果物が taskd に登録されていれば通る。 例:{" "}
              <code className="rounded bg-surface-2 px-1 py-0.5 font-mono text-xs">
                {'{"type":"artifact_exists","name":"bench.json"}'}
              </code>
            </dd>
          </div>
          <div>
            <dt>
              <code className="rounded bg-surface-2 px-1.5 py-0.5 font-mono text-xs font-semibold text-fg">
                reviewer
              </code>
            </dt>
            <dd className="mt-1 text-fg-muted">
              別立てのレビュー run（自動）が判定する。人間の承認ではない。 例:{" "}
              <code className="rounded bg-surface-2 px-1 py-0.5 font-mono text-xs">
                {'{"type":"reviewer","text":"the diff is minimal"}'}
              </code>
            </dd>
          </div>
          <div>
            <dt>
              <code className="rounded bg-surface-2 px-1.5 py-0.5 font-mono text-xs font-semibold text-fg">human</code>
            </dt>
            <dd className="mt-1 text-fg-muted">
              人間が Approval の子タスクで承認／却下して判定する（
              <Link
                to="/"
                className="text-primary underline decoration-primary/40 underline-offset-2 hover:decoration-primary"
              >
                受信箱
              </Link>
              の「承認待ち」に出る）。 例:{" "}
              <code className="rounded bg-surface-2 px-1 py-0.5 font-mono text-xs">
                {'{"type":"human","text":"reviewer is happy"}'}
              </code>
            </dd>
          </div>
        </dl>
      </Section>

      <Section id="status" icon="activity" tone="info" heading="状態" testId="help-status-section">
        <div className="overflow-x-auto rounded-lg border border-border">
          <table className={tableClass}>
            <thead className={theadClass}>
              <tr>
                <th className={thClass}>状態</th>
                <th className={thClass}>意味</th>
                <th className={thClass}>人間ができること</th>
              </tr>
            </thead>
            <tbody>
              {STATUS_ROWS.map((row) => (
                <tr key={row.status} className={trHoverClass}>
                  <td className={tdClass}>
                    <StatusBadge status={row.status} />
                  </td>
                  <td className={cn(tdClass, "text-fg-muted")}>{row.meaning}</td>
                  <td className={cn(tdClass, "text-fg-muted")}>{row.canDo}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </Section>

      <Section id="glossary" icon="book" tone="neutral" heading="用語集" testId="help-glossary-section">
        <dl className="grid gap-x-6 gap-y-3 text-sm sm:grid-cols-2">
          {GLOSSARY.map((entry) => (
            <div key={entry.term}>
              <dt className="font-semibold text-fg">{entry.term}</dt>
              <dd className="mt-0.5 text-fg-muted">{entry.text}</dd>
            </div>
          ))}
        </dl>
      </Section>

      <Section id="trouble" icon="help" tone="warning" heading="困ったとき" testId="help-trouble-section">
        <dl className="divide-y divide-border text-sm">
          <div className="py-3 first:pt-0">
            <dt className="font-semibold text-fg">taskd が止まっている</dt>
            <dd className="mt-0.5 text-fg-muted">
              画面上部に「taskd に接続できません」という赤い帯が出て操作できなくなる。5
              秒ごとに自動で再接続を試みるので、taskd を起動すれば自動で消える。
            </dd>
          </div>
          <div className="py-3">
            <dt className="font-semibold text-fg">401</dt>
            <dd className="mt-0.5 text-fg-muted">
              taskd への認証（トークン）が無い・違う場合はバナーで知らせる。GUI
              自身のログインが切れている場合、通常のページはログイン画面に 戻るが、SSE や成果物の取得はその場で 401
              になる。
            </dd>
          </div>
          <div className="py-3">
            <dt className="font-semibold text-fg">401（プロバイダ・アカウントの追加/編集/削除）</dt>
            <dd className="mt-0.5 text-fg-muted">
              管理系 API（プロバイダ・アカウントの追加/編集/削除、reload）はトークンが必須。
              <code>TASKD_API_TOKEN_FILE</code> を taskd の <code>[api] token_file</code> と同じ内容にして GUI
              を再起動する。
            </dd>
          </div>
          <div className="py-3">
            <dt className="font-semibold text-fg">403</dt>
            <dd className="mt-0.5 text-fg-muted">
              不正なリクエスト元（CSRF）として拒否された、またはファイルの参照先がワークスペースの外に出ている。
            </dd>
          </div>
          <div className="py-3">
            <dt className="font-semibold text-fg">409</dt>
            <dd className="mt-0.5 text-fg-muted">
              他の人・他のタブが先に状態を変えた、またはその状態ではその操作ができない。画面が最新の状態に更新されるので、それを見て操作をやり直す。
            </dd>
          </div>
          <div className="py-3">
            <dt className="font-semibold text-fg">422</dt>
            <dd className="mt-0.5 text-fg-muted">
              入力内容が taskd の検証に落ちた。フォームの該当欄の下にメッセージが出る。
            </dd>
          </div>
          <div className="py-3">
            <dt className="font-semibold text-fg">run のログと成果物の見方</dt>
            <dd className="mt-0.5 text-fg-muted">
              タスク詳細の run 一覧から個々の run
              を開くと標準出力・標準エラー・判定結果が見える。成果物はタスク詳細の一覧から開く／保存する。
            </dd>
          </div>
          <div className="py-3 last:pb-0">
            <dt className="font-semibold text-fg">docs/taskd-requests.md に書く場面</dt>
            <dd className="mt-0.5 text-fg-muted">
              taskd の応答が `docs/taskd-api-v1.md` の記載と違う、または足りないと分かったとき、GUI
              側の開発者がそこに現象と証拠を記録して taskd 側に依頼する（GUI では回避しない）。
            </dd>
          </div>
        </dl>
      </Section>
    </div>
  );
}

/** 節（3 分で分かる流れ・画面ごとの説明・…）の共通の見た目。見出しの id・文字列、section の data-testid は呼び出し側が渡す。 */
function Section({
  id,
  icon,
  tone,
  heading,
  testId,
  children,
}: {
  id: string;
  icon: IconName;
  tone: Tone;
  heading: string;
  testId: string;
  children: ReactNode;
}) {
  return (
    <section aria-labelledby={id} data-testid={testId} className="scroll-mt-20">
      <Card>
        <div className="flex items-center gap-3 border-b border-border px-5 py-4">
          <span className={cn("grid size-8 shrink-0 place-items-center rounded-lg", TONE_ICON_WRAP[tone])}>
            <Icon name={icon} className="size-4" />
          </span>
          <h2 id={id} className="text-[0.95rem] font-semibold text-fg">
            {heading}
          </h2>
        </div>
        <CardBody>{children}</CardBody>
      </Card>
    </section>
  );
}
