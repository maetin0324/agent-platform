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
    text: "ワーカーが taskd に明示的に登録したファイル（ディスクを自動スキャンして拾うことはしない）。SPEC §2.2「調査の案件を投げる — 終わったとき、GUI から調査結果の文書と見るべき関連研究へのリンクがまとまって読める」。/artifacts で案件を横断して一覧でき、Markdown（report.md 等）はその場で描画、sources.json はリンク集（url・title・引用の有無）として、その他の JSON は整形表示する。コードの置き場所（タスクの workspace）は SPEC §3.7「コードは ~/workspace/… のリポジトリ」どおり、リンクではなくコピー用のパス表示（ローカルなら vscode で開くリンクも添える）。",
  },
  {
    term: "分野（genre）",
    text: "タスクが属する専門領域（例: コーディング、関連研究調査）。設定の [[genres]] にある id で、どのハーネス・役割の集団に作業を投げるかの入口になる（実際にどのアダプタで動くかは、その分野の既定役割などが持つ）。委譲できる親タスクには使える分野と役割の一覧が渡され、子タスクを別の分野に委譲できる。分野は「できること」（capabilities）と「渡すもの／返るもの」（input_artifacts / output_artifacts）も広告でき、Plannerや委譲する親タスクはどこに作業を送るか選ぶときにその一覧を見る。",
  },
  {
    term: "接続方式（auth）",
    text: "クラスタ（[[clusters]]）ごとに設定する、接続の張り方。manual（既定）は taskd が自分では接続を張らず、人が手元で scripts/cluster-login.sh を実行する。publickey は鍵だけで入れるクラスタで、クラスタ画面の「接続」ボタンを押すだけで taskd が張る（ディスパッチャが自動でも試みる）。totp は publickey の後に検証コード（2 要素認証）が要るクラスタで、「接続」→ 表示されたプロンプトを見て検証コードを入力 →「送信」。コードはその場で ssh に渡すだけで taskd には保存されず、ログにも画面にも残らない。",
  },
  {
    term: "組織",
    text: "SPEC §3.2「組織（一つ、役割の木）」。秘書を根に部・課へと分かれる、たった一つの役割の木。木の一つ一つが人（担当）で、長期記憶を持ち、複数の案件の仕事を並列に抱える。/org の組織の木から役職を足す・分ける・消すことができる。",
  },
  {
    term: "案件",
    text: "SPEC §3.3「案件は組織の上から入り、分解されて下へ流れる」。曖昧な依頼文のままで投げてよい。秘書が理解の確認・方針・最初の途中目標を返し、途中目標ごとの達成を判定しながらアジャイルに進む。",
  },
  {
    term: "作業場所",
    text: "ADR-0039「案件が作業場所（コードのある場所）を持ち、計画・委譲の子がそれを継ぐ」。案件の新規フォーム・秘書の「新しい案件として」で「手元」（普段のパス、例: ~/workspace/rust/…。~ は展開して保存される）／「クラスタ」（GET /clusters から選ぶ + リモートの作業ディレクトリ）／「まだ決めない」を選べる。案件画面の「作業場所」カードでいつでも編集・消去でき、未設定のままだと分解した仕事が空の作業ディレクトリに置かれてしまう（実機の事故 2026-09-18: ワーカーが自分で ssh してリポジトリを探しに行った）。この作業場所は分解（この方針で進める）とその子タスクが継ぐ（明示 > 案件 > 親）。Remote のときは「置き場所」に cluster:path に加えて手元の写し（workspace_dir）も出る（編集は手元、検証はリモートで行う）。",
  },
  {
    term: "途中目標（milestone）",
    text: "SPEC §7「途中目標は予め大まかに決めておいて、適宜再設計する」。案件の中の通し番号付きの区切りで、提案 → 承認済み → 進行中 → 達成（または再設計）と進む。仕事が止まると秘書が結果をまとめて次の途中目標を提案し（ADR-0038）、案件画面のカードで「ok」（達成にして次を承認し分解を頼む）／「議論」（一言を秘書へ送って対話を続ける）／「ng」（達成にせず再設計を頼む。理由が必須）を判定する。既存の状態を直接変えるボタンは裏方の詳細（「状態を直接変える」）に畳んである。",
  },
  {
    term: "秘書",
    text: "SPEC §3.1「あなたの相手。案件を受け取り、組織に流し、報告を集めてあなたに渡す。部をまたぐ連携は秘書が認める」。/org/secretary で話す。案件を投げるのも、状況を聞くのも、方針を変えるのもここから。",
  },
  {
    term: "口出し",
    text: "SPEC §3.4「おかしなことをしていたり、追加の指示を出したいとき、組織の木を見て『誰に言うか』を決め、その担当に直接言う。相手は『人』なので、先週の議論の続きとして話せる」。/org で担当を選んで「話す」、または案件の仕事の木から「担当に話す」。",
  },
  {
    term: "記憶",
    text: "担当が案件をまたいで覚えていること（notes.md）と、案件ごとの引き出し（projects/<案件>.md）。仕事の前に毎回前置きされ、仕事の後に担当自身が書き足す。組織の画面で読める（書き込みはできない。直すならファイルを直接編集する）。",
  },
  {
    term: "対話（messages）",
    text: "担当ごと・案件ごとに分かれたやり取り（古い順）。話しかけると裏で作業が 1 回動き、その結果が返事として入る（だから返事はすぐには返らず、送ったあとは「考え中」のまま待つ）。返事は Markdown として描かれ、その返事を作った裏方のタスクへも辿れる。返事を作れない状態（経路なし等）になったときは、待つのをやめて理由を出す。",
  },
  {
    term: "仕事の木（DAG）",
    text: "パッと見れば、おかしな方針を立てていないかが分かるための図。案件に属する仕事を、親子と依存の辺でそのまま描く。四角を押すと裏方のタスク（/tasks/:id）へ移る。対話の返事や報告のまとめのような裏方の作業は出さない。",
  },
  {
    term: "報告",
    text: "SPEC §3.5「上に行くほど多くのレビューが入り、圧縮される。だから上で見る報告はパッと見の判断が楽」。良い報告の例（あなたの言葉）: 「ここまで作業が進み、この程度の結果が確認できました。またこの結果から、このような framing で論文執筆が可能だと思われます」。kind は progress／result／bad_news／proposal／question。生成は決定的（LLM は呼ばない）、圧縮だけ別 run（レビュアー役）が行う。",
  },
  {
    term: "悪い知らせ",
    text: "「ここの脆弱性がヤバい」「あのマシンが落ちた・壊れた」のような報告。良い知らせと同じ経路で、目立つ形（赤い行）で届く。圧縮を待たず各段を素通りして秘書まで即座に上がる。",
  },
  {
    term: "圧縮",
    text: "SPEC §3.5「通知は数分単位ではなく、数時間単位」。上の担当は、下から上がった報告が溜まると（既定 4 件、または最古が 2 時間経過）まとめの作業を 1 回起こし、下の報告を 1 件にまとめて自分の報告にする（元の報告は展開して辿れる）。秘書まで上がった報告が人の見る報告。まとめの作業は裏方なので仕事の木には出ない。",
  },
  {
    term: "認可",
    text: "SPEC §3.6「少しでも聞くべきだとエージェントが判断したら、あなたに指示を仰ぐ」。担当が作業の途中で人に聞きたいことがあると質問で終わり、それが /approvals の「認可待ち」に並ぶ。答えると、その答えが担当へ渡り、作業が再開する。",
  },
  {
    term: "今回だけ／今後ずっと",
    text: "聞かれたことに「今回だけ」か「同じようなことは今後ずっと」のどちらかで答える。/approvals の 3 つのボタン（今回だけ・今後ずっと・認めない）がこれに当たる。「今後ずっと」を選ぶと、その答えが規則文として永続の認可に 1 行加わる（範囲はこの担当だけ／全員から選ぶ）。「認めない」は答えの先頭に「認めない: 」を付けて担当へ返す。",
  },
  {
    term: "永続の認可",
    text: "SPEC §3.6「永続の認可は文字で記録してエージェントに注入する」。規則文 1 行（担当宛て、または全員宛て）。以後、その宛先の作業に毎回前置きされる。/approvals の下部で一覧・追加・削除ができ、質問を経ずに直接足すこともできる。組織の木（/org）で担当を選んだときにも、その担当への永続の認可が出る。",
  },
  {
    term: "Discord 通知",
    text: "人の判断が要るとき（途中目標の仕事が終わった／認可の要求が来た／質問で止まっている／悪い知らせが届いた／秘書から方針の提案が届いた、の 5 つだけ）に Discord の webhook へ 1 通届く仕組み。結果が出たことは知らせない（それは「報告」の流れで見る）。Webhook URL は秘密として /accounts の「API キー」節に id discord-webhook で登録する。/reports の「通知（Discord）」区画で設定状況の確認・テスト送信・直近 10 件の送信結果を見られる。",
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
    href: "/org/secretary",
    icon: "message",
    title: "秘書",
    text: "SPEC §4「秘書との対話 — 案件を投げる、状況を聞く、方針を変える」。左で案件を選び（「案件なし」は雑談）、下の欄から話しかける。「新しい案件として」で送ると、その本文が新しい案件の依頼になり（先頭 40 字が案件名）、作業場所（手元／クラスタ／まだ決めない）も添えられる。秘書が理解の確認・大まかな方針・最初の途中目標を返す（SPEC §7）。返事は同期では返らないので、送ったあとは「考え中」を出して待つ。",
  },
  {
    href: "/org",
    icon: "users",
    title: "組織",
    text: "SPEC §4「組織の木 — 誰が何を抱えているか」。秘書を根にした部・課の木を見る画面。担当を選ぶとその一言・分野・抱えている仕事が出て、「話す」からその担当に直接言える。役職の追加・変更・削除もここで行う（管理系 API のトークンが要る）。",
  },
  {
    href: "/projects",
    icon: "folder",
    title: "案件",
    text: "案件の一覧・作成と、個々の案件の途中目標・仕事の木を見る画面。新規フォームでは作業場所（手元／クラスタ／まだ決めない）も選べる。案件の詳細には「作業場所」カードがあり、現在値の確認・編集・消去ができる（未設定だと分解した仕事は空の作業ディレクトリで走る）。方針に納得したら「この方針で進める」を押すと、秘書がここまでのやり取りと途中目標をまとめて仕事に分解する（返事は待たない。仕事の木が増えていく）。途中目標は達成のたびに Go を出すか再設計する。",
  },
  {
    href: "/reports",
    icon: "send",
    title: "報告",
    text: "SPEC §4「報告の流れ — 各所から上がってくる報告を高速で流し見する。良い知らせも悪い知らせも」。既定は秘書レベル（level=0）の未読を新しい順に 1 件 1 行で。クリックで展開すると本文と、圧縮の元になった下の段の報告（sources_expanded）を辿れる。既読は 1 件ずつ／一括で。案件詳細（/projects/:id）にも、その案件の全レベルの報告が「報告」タブとして出る。上部の「通知（Discord）」区画では、人の判断が要る 5 種の知らせ（途中目標の達成判定・認可・質問・悪い知らせ・秘書の方針提案）だけを Discord にも届ける設定・テスト送信・直近の送信結果を見られる。",
  },
  {
    href: "/approvals",
    icon: "shield",
    title: "認可",
    text: "SPEC §4「認可の要求 — 聞かれたことに『今回だけ／今後ずっと』で答える。永続の認可の一覧と編集」。上に未決の要求（担当からの質問。同じ文面の要求は 1 枚にまとまり「N 件」と出る）、その下に決めたものの履歴、さらに下に永続の認可の一覧・追加・削除が並ぶ。「今回だけ」「今後ずっと」「認めない」のいずれかで答えると、その担当の作業が再開する。「今後ずっと」で答えると、その答えが規則文として永続の認可に加わり、以後その担当（または全員）に前置きされる。ナビの「認可」には未決の件数のバッジが付く。",
  },
  {
    href: "/artifacts",
    icon: "file",
    title: "成果物",
    text: "SPEC §4「成果物 — 調査文書・リンク集はここで読む。コードは置き場所へのリンク」。案件を選ぶと、その案件のタスクの成果物を横断して一覧できる。Markdown はその場で描画、sources.json はリンク集、その他の JSON は整形表示。各行にタスクの置き場所（workspace）も出る。個々のタスクの成果物は従来どおり /tasks/:id でも見られる。案件詳細（/projects/:id）にも同じ一覧が「成果物」節として出る。",
  },
  {
    href: "/inbox",
    icon: "inbox",
    title: "受信箱（裏方）",
    text: "人間の対応が要る裏方の項目（承認待ち・質問・受け入れ待ちの draft・注意）だけを集めた画面。普段は「秘書」から始め、裏方で詰まったときにここを見る。",
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
