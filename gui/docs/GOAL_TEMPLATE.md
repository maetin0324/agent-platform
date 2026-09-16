docs/DESIGN.md §10 の Phase G__N__ の受け入れ条件が全て満たされ、次の証拠が会話に示されている:
- `pnpm lint`、`pnpm typecheck`、`pnpm test`、`pnpm build` が exit 0（テスト数を報告）。G1 以降は `pnpm e2e`（Playwright、実 taskd）も exit 0（シナリオ数を報告）
- `pnpm gen:types && git diff --exit-code app/taskd/types.ts` が差分ゼロ
- auditor サブエージェントの監査報告が「可」または「条件付き可」で、「不可」の項目がゼロ
- docs/PROGRESS.md に「## Phase G__N__ — DONE」という見出しの節があり、受け入れ条件ごとの証拠、監査結果、未解決事項、提案、taskd への依頼が書かれている
- `git status` がクリーンで、直近のコミットメッセージが "phase G__N__:" で始まる

進め方の制約:
- まず docs/PROGRESS.md と docs/adr/ を読み、現在地を確認してから始める。前のフェーズの未解決事項があれば PROGRESS.md に引き継ぎとして残すだけで、このフェーズでは対処しない
- 最初に `scripts/taskd.sh build && scripts/taskd.sh start dev` を実行し、`curl -s http://127.0.0.1:7710/api/v1/health` が `api_version: "1"` を返すことを確認する（G0 ではこれ自体が受け入れ条件）。返さなければ docs/taskd-requests.md に状況を書き、「## Phase G__N__ — BLOCKED」を PROGRESS.md に書いてコミットし、止まる
- 作業を始める前に、このフェーズの作業を「互いにファイルを共有しない独立した単位」に分ける。独立した単位が 2 つ以上あるときだけ implementer サブエージェントを並列に使う（同時に最大 3）。順序依存のある作業、設計判断を含む作業、1 ファイルに収まる作業は自分で行う。サブエージェントを起動する前に、単位ごとの担当ファイルを明示する
- サブエージェントの報告に「判断が必要な点」があれば自分で判断し、ADR が必要なら docs/adr/ に追加する
- 全テストが通ったら auditor サブエージェントを 1 回だけ起動する。「不可」があれば修正し、修正後の再監査は自分で行う（auditor の再起動は最大 1 回）
- テストで外部ネットワークに出ない（`pnpm install` と `pnpm exec playwright install chromium` の準備だけが例外）。結合テストは `scripts/taskd.sh` が起動した実 taskd（fake ワーカー）に対して行う
- taskd の API が足りない・仕様（docs/taskd-api-v1.md）と違うと分かったら、GUI 側で回避せず（SQLite を開かない、taskctl の出力を解析しない、派生値を再計算しない）、docs/taskd-requests.md に「エンドポイント / 期待 / 実際（curl の出力）/ できないこと」を書き、「## Phase G__N__ — BLOCKED」を PROGRESS.md に書いてコミットし、止まる
- docs/DESIGN.md と docs/taskd-api-v1.md は編集しない。変更提案は PROGRESS.md の「提案」節に書く
- 同じ失敗が 3 回続いたら、そのアプローチをやめて PROGRESS.md に状況を書き、別のアプローチを 1 つだけ試す。それも失敗したら「## Phase G__N__ — BLOCKED」を PROGRESS.md に書いてコミットし、止まる
- 経過ターン数を毎ターン一言で報告し、__MAXTURNS__ ターンに達したら、その時点の状態を PROGRESS.md に「## Phase G__N__ — PARTIAL」として書いてコミットし、止まる
- 終了前に `scripts/taskd.sh stop dev`（と、起動した他の name）で taskd を止める
