import { Link } from "react-router";
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
  { status: "blocked", meaning: "ワーカーが人間への質問を残して止まっている。", canDo: "回答／取り消し" },
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
  { term: "cooldown", text: "プロバイダがスロットル等で一時的に使えない期間。解けると自動で再び使われる。" },
  {
    term: "Plan",
    text: "複数の子タスクをまとめる親タスク（kind=plan）。子タスクは draft で作られ、人間が受け入れて進める。",
  },
  { term: "Approval", text: "人間の承認を待つための子タスク（kind=approval）。承認／却下で親の判定が決まる。" },
  {
    term: "成果物",
    text: "ワーカーが taskd に明示的に登録したファイル（ディスクを自動スキャンして拾うことはしない）。",
  },
];

export default function HelpPage() {
  return (
    <div className="max-w-3xl space-y-10">
      <h1 className="text-xl font-semibold">使い方</h1>

      <nav aria-label="使い方の目次" className="text-sm">
        <ul className="flex flex-wrap gap-x-4 gap-y-1 text-blue-700">
          <li>
            <a href="#flow" className="hover:underline">
              3 分で分かる流れ
            </a>
          </li>
          <li>
            <a href="#screens" className="hover:underline">
              画面ごとの説明
            </a>
          </li>
          <li>
            <a href="#acceptance" className="hover:underline">
              受け入れ条件
            </a>
          </li>
          <li>
            <a href="#status" className="hover:underline">
              状態
            </a>
          </li>
          <li>
            <a href="#glossary" className="hover:underline">
              用語集
            </a>
          </li>
          <li>
            <a href="#trouble" className="hover:underline">
              困ったとき
            </a>
          </li>
        </ul>
      </nav>

      <section aria-labelledby="flow" data-testid="help-flow-section">
        <h2 id="flow" className="text-lg font-semibold">
          3 分で分かる流れ
        </h2>
        <ol className="mt-2 list-decimal space-y-1 pl-5 text-sm">
          <li>
            <Link to="/tasks/new" className="hover:underline">
              タスクを作る
            </Link>
            （または{" "}
            <Link to="/plans/new" className="hover:underline">
              Plan を作る
            </Link>
            ）
          </li>
          <li>人間が承認する（draft を受け入れる。kind=approval のタスクは承認／却下で判定する）</li>
          <li>taskd がワーカーを起動する（人間は何もしない。順番・タイミングは taskd が決める）</li>
          <li>taskd が受け入れ条件を自分で判定する（ワーカーの自己申告は信じない。再実行して確かめる）</li>
          <li>条件を満たせば done。満たせなければ requeue して再試行するか failed になる</li>
        </ol>
        <p className="mt-2 text-sm text-gray-600">
          人間が触るのは<strong>承認・回答・取り消し</strong>だけ。何をいつ動かすかは taskd が決める。
        </p>
      </section>

      <section aria-labelledby="screens" data-testid="help-screens-section">
        <h2 id="screens" className="text-lg font-semibold">
          画面ごとの説明
        </h2>
        <dl className="mt-2 space-y-3 text-sm">
          <div>
            <dt className="font-semibold">
              <Link to="/" className="hover:underline">
                受信箱
              </Link>
            </dt>
            <dd>
              人間の対応が要る項目（承認待ち・質問・受け入れ待ちの draft・注意）だけを集めた画面。まずここを開く。
            </dd>
          </div>
          <div>
            <dt className="font-semibold">
              <Link to="/tasks" className="hover:underline">
                一覧
              </Link>
            </dt>
            <dd>全タスクを状態・条件で絞り込んで見る画面。特定のタスクを探すときに開く。</dd>
          </div>
          <div>
            <dt className="font-semibold">詳細（/tasks/&lt;id&gt;）</dt>
            <dd>
              1
              件のタスクの受け入れ条件・run・イベントの流れ・成果物を見る画面。一覧や受信箱から個々のタスクを開くと表示される。
            </dd>
          </div>
          <div>
            <dt className="font-semibold">
              <Link to="/graph" className="hover:underline">
                DAG
              </Link>
            </dt>
            <dd>タスク同士の依存関係と Plan の親子関係を図で見る画面。全体の進み具合を俯瞰したいときに開く。</dd>
          </div>
          <div>
            <dt className="font-semibold">
              <Link to="/providers" className="hover:underline">
                プロバイダ
              </Link>
            </dt>
            <dd>
              各アカウントの利用状況（done / requeue
              の件数、トークン、cooldown）を見る画面。動きが遅い・偏っていると感じたら開く。
            </dd>
          </div>
          <div>
            <dt className="font-semibold">
              <Link to="/daemon" className="hover:underline">
                デーモン
              </Link>
            </dt>
            <dd>
              taskd 本体の状態（pid・tick・実行中の run・承認待ち・経路なしのタスク）と replay を見る画面。taskd
              自体の様子を確認したいときに開く。
            </dd>
          </div>
        </dl>
      </section>

      <section aria-labelledby="acceptance" data-testid="help-acceptance-section">
        <h2 id="acceptance" className="text-lg font-semibold">
          受け入れ条件
        </h2>
        <p className="mt-2 text-sm text-gray-600">
          タスクの受け入れ条件（acceptance）は 4 種類。<strong>判定は taskd が自分で再実行して確かめる</strong>
          （ワーカーが「テストを通した」と言っても、それだけでは信じない）。
        </p>
        <dl className="mt-2 space-y-2 text-sm">
          <div>
            <dt className="font-mono font-semibold">command</dt>
            <dd>
              シェルコマンドを実行し、終了コードが期待値（既定 0）と一致すれば通る。 例:{" "}
              <code className="text-xs">{'{"type":"command","cmd":"cargo test","expect_exit":0}'}</code>
            </dd>
          </div>
          <div>
            <dt className="font-mono font-semibold">artifact_exists</dt>
            <dd>
              指定した名前の成果物が taskd に登録されていれば通る。 例:{" "}
              <code className="text-xs">{'{"type":"artifact_exists","name":"bench.json"}'}</code>
            </dd>
          </div>
          <div>
            <dt className="font-mono font-semibold">reviewer</dt>
            <dd>
              別立てのレビュー run（自動）が判定する。人間の承認ではない。 例:{" "}
              <code className="text-xs">{'{"type":"reviewer","text":"the diff is minimal"}'}</code>
            </dd>
          </div>
          <div>
            <dt className="font-mono font-semibold">human</dt>
            <dd>
              人間が Approval の子タスクで承認／却下して判定する（
              <Link to="/" className="hover:underline">
                受信箱
              </Link>
              の「承認待ち」に出る）。 例:{" "}
              <code className="text-xs">{'{"type":"human","text":"reviewer is happy"}'}</code>
            </dd>
          </div>
        </dl>
      </section>

      <section aria-labelledby="status" data-testid="help-status-section">
        <h2 id="status" className="text-lg font-semibold">
          状態
        </h2>
        <table className="mt-2 w-full text-left text-sm">
          <thead>
            <tr className="text-xs text-gray-500">
              <th className="pr-2">状態</th>
              <th className="pr-2">意味</th>
              <th className="pr-2">人間ができること</th>
            </tr>
          </thead>
          <tbody>
            {STATUS_ROWS.map((row) => (
              <tr key={row.status}>
                <td className="pr-2 font-mono">{row.status}</td>
                <td className="pr-2">{row.meaning}</td>
                <td className="pr-2">{row.canDo}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>

      <section aria-labelledby="glossary" data-testid="help-glossary-section">
        <h2 id="glossary" className="text-lg font-semibold">
          用語集
        </h2>
        <dl className="mt-2 space-y-2 text-sm">
          {GLOSSARY.map((entry) => (
            <div key={entry.term}>
              <dt className="font-semibold">{entry.term}</dt>
              <dd>{entry.text}</dd>
            </div>
          ))}
        </dl>
      </section>

      <section aria-labelledby="trouble" data-testid="help-trouble-section">
        <h2 id="trouble" className="text-lg font-semibold">
          困ったとき
        </h2>
        <dl className="mt-2 space-y-2 text-sm">
          <div>
            <dt className="font-semibold">taskd が止まっている</dt>
            <dd>
              画面上部に「taskd に接続できません」という赤い帯が出て操作できなくなる。5
              秒ごとに自動で再接続を試みるので、taskd を起動すれば自動で消える。
            </dd>
          </div>
          <div>
            <dt className="font-semibold">401</dt>
            <dd>
              taskd への認証（トークン）が無い・違う場合はバナーで知らせる。GUI
              自身のログインが切れている場合、通常のページはログイン画面に 戻るが、SSE や成果物の取得はその場で 401
              になる。
            </dd>
          </div>
          <div>
            <dt className="font-semibold">403</dt>
            <dd>不正なリクエスト元（CSRF）として拒否された、またはファイルの参照先がワークスペースの外に出ている。</dd>
          </div>
          <div>
            <dt className="font-semibold">409</dt>
            <dd>
              他の人・他のタブが先に状態を変えた、またはその状態ではその操作ができない。画面が最新の状態に更新されるので、それを見て操作をやり直す。
            </dd>
          </div>
          <div>
            <dt className="font-semibold">422</dt>
            <dd>入力内容が taskd の検証に落ちた。フォームの該当欄の下にメッセージが出る。</dd>
          </div>
          <div>
            <dt className="font-semibold">run のログと成果物の見方</dt>
            <dd>
              タスク詳細の run 一覧から個々の run
              を開くと標準出力・標準エラー・判定結果が見える。成果物はタスク詳細の一覧から開く／保存する。
            </dd>
          </div>
          <div>
            <dt className="font-semibold">docs/taskd-requests.md に書く場面</dt>
            <dd>
              taskd の応答が `docs/taskd-api-v1.md` の記載と違う、または足りないと分かったとき、GUI
              側の開発者がそこに現象と証拠を記録して taskd 側に依頼する（GUI では回避しない）。
            </dd>
          </div>
        </dl>
      </section>
    </div>
  );
}
