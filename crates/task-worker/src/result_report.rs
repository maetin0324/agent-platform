//! 結果ファイルの `report`（ADR-0034 D7。Phase 27）。
//!
//! ワーカーは `done` を返すとき、その結果が「提案」なのか「ただの結果」なのかを**自分で宣言**できる:
//!
//! ```json
//! {"summary": "…", "evidence": [], "report": {"kind": "proposal"}}
//! ```
//!
//! ここは**ファイルを読んで文字列を取り出すだけ**で、`ReportKind` への写し替え（固定表）は
//! `task-dispatch` 側が行う。判断（この結果が提案に値するか）は celeris ではしない（DESIGN 原則 1）。
//! `memory`（ADR-0033 D6）と同じ流儀: 無い・JSON でない・形が違うときは `None`（run は失敗させない）。

use std::path::Path;

/// 結果ファイルの `report`（`kind` だけ。未知の値もそのまま文字列で持つ）。
#[derive(
    Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize, schemars::JsonSchema,
)]
pub struct ReportDeclaration {
    /// `"result"` / `"proposal"` / `"bad_news"` / `"question"`。欠落・未知の値は呼び出し側が既定に倒す。
    #[serde(default)]
    pub kind: Option<String>,
}

/// 結果ファイル（`<artifacts_dir>/result.json`。ADR-0036 D2）の `report.kind`。
pub fn read_result_report_kind(artifacts_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(artifacts_dir.join("result.json")).ok()?;
    report_kind_from_result_json(&text)
}

/// 結果ファイルの本文から `report.kind` を取り出す（純粋関数）。
pub fn report_kind_from_result_json(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let declaration: ReportDeclaration =
        serde_json::from_value(value.get("report")?.clone()).ok()?;
    let kind = declaration.kind?;
    if kind.trim().is_empty() {
        None
    } else {
        Some(kind)
    }
}

/// 結果ファイルの `milestone_proposal`（ADR-0038 D1。Phase 41）。対話 run（途中目標のレビュー）が
/// 「次の途中目標」を宣言するための、`report.kind` と同じ形の宣言的フィールド:
///
/// ```json
/// {"summary": "…", "evidence": [], "milestone_proposal": {"title": "…", "description": "…"}}
/// ```
///
/// ここも**ファイルを読んで文字列を取り出すだけ**で、`milestones` に行を作るのは celeris 側（決定的）。
#[derive(
    Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize, schemars::JsonSchema,
)]
pub struct MilestoneProposal {
    pub title: String,
    #[serde(default)]
    pub description: String,
}

/// 結果ファイル（`<artifacts_dir>/result.json`）の `milestone_proposal`。
pub fn read_result_milestone_proposal(artifacts_dir: &Path) -> Option<MilestoneProposal> {
    let text = std::fs::read_to_string(artifacts_dir.join("result.json")).ok()?;
    milestone_proposal_from_result_json(&text)
}

/// 結果ファイルの `actions`（ADR-0048 D3。Phase 60b）: CoS の対話 run が宣言する、taskd が決定的に
/// 実行する操作。`milestone_proposal` と同じ流儀（ADR-0034 D7）で、ここは**読んで写すだけ**（検証・
/// 実行は `task_ops::actions`）。形（`task_core::ConsoleAction`）は `task-ops` も読めるよう
/// `task-core` に置いてある（`task-worker` はファイル I/O、`task-ops` はストアの読み書き）:
///
/// ```json
/// {"summary": "…", "actions": [
///   {"type": "create_task", "title": "…", "objective": "…", "acceptance": [...], "harness": "coding",
///    "skills": ["rust"], "mode": "prototype", "repos": ["agent-platform"], "project": "<id or null>",
///    "milestone": "<id or null>", "assignee": null},
///   {"type": "propose_project", "title": "…", "request": "…", "repos": [...]},
///   {"type": "add_milestone", "project": "<id>", "title": "…", "description": "…"},
///   {"type": "ask_human", "text": "…"}
/// ]}
/// ```
pub use task_core::ConsoleAction;

/// `actions_from_result_json` の結果。JSON としては読めたが `ConsoleAction` の形に合わない要素は
/// `malformed` に理由付きで残す（`{"type":"create_task"}` のような、必須フィールドが欠けたものなど）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedActions {
    pub valid: Vec<ConsoleAction>,
    /// 形が合わなかった要素の説明（`"action #2: missing field `title`"` のような 1 行）。
    pub malformed: Vec<String>,
}

impl ParsedActions {
    pub fn is_empty(&self) -> bool {
        self.valid.is_empty() && self.malformed.is_empty()
    }
}

/// 結果ファイル（`<artifacts_dir>/result.json`）の `actions`。
pub fn read_result_actions(artifacts_dir: &Path) -> ParsedActions {
    let Ok(text) = std::fs::read_to_string(artifacts_dir.join("result.json")) else {
        return ParsedActions::default();
    };
    actions_from_result_json(&text)
}

/// 結果ファイルの本文から `actions` を取り出す（純粋関数）。`actions` が無い・JSON でない・配列でない
/// ときは空（malformed も無し）。要素ごとに `ConsoleAction` へ変換を試み、失敗したものは `malformed` に残す
/// （taskd はそれを「実行できなかった action」として人に見せる）。
pub fn actions_from_result_json(text: &str) -> ParsedActions {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return ParsedActions::default();
    };
    let Some(items) = value.get("actions").and_then(|v| v.as_array()) else {
        return ParsedActions::default();
    };
    let mut out = ParsedActions::default();
    for (i, item) in items.iter().enumerate() {
        match serde_json::from_value::<ConsoleAction>(item.clone()) {
            Ok(action) => out.valid.push(action),
            Err(e) => out.malformed.push(format!("action #{}: {e}", i + 1)),
        }
    }
    out
}

/// ADR-0054 Phase 112 D3: 対話 run が `result.json` を書けず（`artifacts_dir` に書き込めない等）、
/// 最終メッセージの本文にその代わりを吐いたと思われるときの判定。`codex` の「対話かつ `result.json`
/// 不在なら最終メッセージをそのまま `Done.summary` にする」救済（ADR-0049）の**手前**で使う純粋関数
/// （I/O をしない。判定だけ）。テキストが JSON として parse でき、空でない `summary` 文字列と
/// `actions` 配列の両方を持つ形なら「recoverable」（`true`）。それ以外（壊れた JSON・ただの平文回答・
/// `actions` の無い結果ファイル）は `false`（呼び出し側は従来どおり生テキストをそのまま `Done.summary`
/// にする）。`true` のとき、呼び出し側はこの同じテキストを `<artifacts_dir>/result.json` として書き
/// 直し、`actions`/`summary` の検証は既存の（disk から読む）経路に任せる — ここでは検証しない。
pub fn final_message_is_recoverable_result(text: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    let has_summary = value
        .get("summary")
        .and_then(|s| s.as_str())
        .is_some_and(|s| !s.trim().is_empty());
    let has_actions = value.get("actions").is_some_and(|a| a.is_array());
    has_summary && has_actions
}

/// 結果ファイルの本文から `milestone_proposal` を取り出す（純粋関数）。`title` が空なら提案なし。
pub fn milestone_proposal_from_result_json(text: &str) -> Option<MilestoneProposal> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let proposal: MilestoneProposal =
        serde_json::from_value(value.get("milestone_proposal")?.clone()).ok()?;
    if proposal.title.trim().is_empty() {
        return None;
    }
    Some(MilestoneProposal {
        title: proposal.title.trim().to_string(),
        description: proposal.description.trim().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_declared_kind_is_read_and_anything_else_is_none() {
        assert_eq!(
            report_kind_from_result_json(r#"{"summary":"s","report":{"kind":"proposal"}}"#)
                .as_deref(),
            Some("proposal")
        );
        // 未知の値もそのまま返す（固定表に無い値を既定に倒すのは呼び出し側）。
        assert_eq!(
            report_kind_from_result_json(r#"{"summary":"s","report":{"kind":"bogus"}}"#).as_deref(),
            Some("bogus")
        );
        // 無い・空・形違い・JSON でない。
        assert_eq!(report_kind_from_result_json(r#"{"summary":"s"}"#), None);
        assert_eq!(report_kind_from_result_json(r#"{"report":{}}"#), None);
        assert_eq!(
            report_kind_from_result_json(r#"{"report":{"kind":"  "}}"#),
            None
        );
        assert_eq!(
            report_kind_from_result_json(r#"{"report":"proposal"}"#),
            None
        );
        assert_eq!(report_kind_from_result_json("not json"), None);
    }

    #[test]
    fn a_missing_result_file_is_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        // ADR-0036 D2: 読むのは成果物ディレクトリの `result.json`（共有 workspace では `.taskd/artifacts/<id>/`）。
        let artifacts = dir.path().join(".taskd/artifacts/01HTASK");
        assert_eq!(read_result_report_kind(&artifacts), None);
        std::fs::create_dir_all(&artifacts).expect("mkdir");
        std::fs::write(
            artifacts.join("result.json"),
            r#"{"summary":"s","report":{"kind":"proposal"}}"#,
        )
        .expect("write");
        assert_eq!(
            read_result_report_kind(&artifacts).as_deref(),
            Some("proposal")
        );
    }
}

#[cfg(test)]
mod recoverable_result_tests {
    use super::*;

    /// ADR-0054 Phase 112 D3: `summary`（空でない文字列）と `actions`（配列）の両方があれば recoverable。
    #[test]
    fn a_summary_and_actions_shaped_message_is_recoverable() {
        assert!(final_message_is_recoverable_result(
            r#"{"summary":"やります","actions":[{"type":"create_task","title":"t","objective":"o"}]}"#
        ));
        // `actions` は空配列でもよい（「配列を持つ」という条件そのもの）。
        assert!(final_message_is_recoverable_result(
            r#"{"summary":"やります","actions":[]}"#
        ));
    }

    /// `actions` が無い・配列でない・`summary` が空/無い・壊れた JSON・ただの平文はどれも recoverable でない。
    #[test]
    fn anything_else_is_not_recoverable() {
        assert!(!final_message_is_recoverable_result(
            r#"{"summary":"やります"}"#
        ));
        assert!(!final_message_is_recoverable_result(
            r#"{"summary":"やります","actions":"nope"}"#
        ));
        assert!(!final_message_is_recoverable_result(
            r#"{"summary":"  ","actions":[]}"#
        ));
        assert!(!final_message_is_recoverable_result(r#"{"actions":[]}"#));
        assert!(!final_message_is_recoverable_result("not json"));
        assert!(!final_message_is_recoverable_result("接続確認OK"));
    }
}

#[cfg(test)]
mod milestone_proposal_tests {
    use super::*;

    /// ADR-0038 D1: 次の途中目標の提案は結果ファイルの宣言的フィールドから読む（LLM の文面は解釈しない）。
    #[test]
    fn the_declared_proposal_is_read_and_anything_else_is_none() {
        let proposal = milestone_proposal_from_result_json(
            r#"{"summary":"s","milestone_proposal":{"title":" 候補の絞り込み ","description":" 3 本に絞る "}}"#,
        )
        .expect("proposal");
        assert_eq!(proposal.title, "候補の絞り込み");
        assert_eq!(proposal.description, "3 本に絞る");
        // description は任意。
        assert_eq!(
            milestone_proposal_from_result_json(r#"{"milestone_proposal":{"title":"次"}}"#)
                .expect("proposal")
                .description,
            ""
        );
        // 無い・空の題名・形違い・JSON でない。
        assert_eq!(
            milestone_proposal_from_result_json(r#"{"summary":"s"}"#),
            None
        );
        assert_eq!(
            milestone_proposal_from_result_json(r#"{"milestone_proposal":{"title":"  "}}"#),
            None
        );
        assert_eq!(
            milestone_proposal_from_result_json(r#"{"milestone_proposal":"次"}"#),
            None
        );
        assert_eq!(milestone_proposal_from_result_json("not json"), None);
    }

    #[test]
    fn a_missing_result_file_has_no_proposal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifacts = dir.path().join(".taskd/artifacts/01HTASK");
        assert_eq!(read_result_milestone_proposal(&artifacts), None);
        std::fs::create_dir_all(&artifacts).expect("mkdir");
        std::fs::write(
            artifacts.join("result.json"),
            r#"{"summary":"s","milestone_proposal":{"title":"次の途中目標","description":"d"}}"#,
        )
        .expect("write");
        assert_eq!(
            read_result_milestone_proposal(&artifacts)
                .expect("proposal")
                .title,
            "次の途中目標"
        );
    }
}

#[cfg(test)]
mod actions_tests {
    use super::*;

    /// ADR-0048 D3: CoS の結果ファイルの `actions` を宣言どおりの形で読む（LLM の文面は解釈しない）。
    #[test]
    fn every_action_type_parses_from_its_declared_json_shape() {
        let parsed = actions_from_result_json(
            r#"{"summary":"s","actions":[
                {"type":"create_task","title":"直す","objective":"直して","acceptance":["直った"],
                 "harness":"coding","skills":["rust"],"mode":"prototype","repos":["agent-platform"],
                 "project":"01P","milestone":"01M","assignee":"engineering"},
                {"type":"propose_project","title":"新案件","request":"やりたい","repos":["/tmp/x"]},
                {"type":"add_milestone","project":"01P","title":"次","description":"説明"},
                {"type":"ask_human","text":"どちらがよいですか"}
            ]}"#,
        );
        assert!(parsed.malformed.is_empty(), "{:?}", parsed.malformed);
        assert_eq!(parsed.valid.len(), 4);
        assert_eq!(parsed.valid[0].kind(), "create_task");
        assert_eq!(parsed.valid[1].kind(), "propose_project");
        assert_eq!(parsed.valid[2].kind(), "add_milestone");
        assert_eq!(parsed.valid[3].kind(), "ask_human");
        match &parsed.valid[0] {
            ConsoleAction::CreateTask {
                title,
                objective,
                acceptance,
                harness,
                skills,
                mode,
                repos,
                project,
                milestone,
                assignee,
                ..
            } => {
                assert_eq!(title, "直す");
                assert_eq!(objective, "直して");
                assert_eq!(acceptance, &vec!["直った".to_string()]);
                assert_eq!(harness.as_deref(), Some("coding"));
                assert_eq!(skills, &vec!["rust".to_string()]);
                assert_eq!(mode.as_deref(), Some("prototype"));
                assert_eq!(repos, &vec!["agent-platform".to_string()]);
                assert_eq!(project.as_deref(), Some("01P"));
                assert_eq!(milestone.as_deref(), Some("01M"));
                assert_eq!(assignee.as_deref(), Some("engineering"));
            }
            other => panic!("expected create_task, got {other:?}"),
        }
    }

    /// 省略できるフィールド（`acceptance` / `harness` / `skills` / `mode` / `repos` / `project` /
    /// `milestone` / `assignee`）は既定値で読める。
    #[test]
    fn create_task_omitted_fields_default_to_empty_or_none() {
        let parsed = actions_from_result_json(
            r#"{"actions":[{"type":"create_task","title":"t","objective":"o"}]}"#,
        );
        assert_eq!(parsed.valid.len(), 1);
        match &parsed.valid[0] {
            ConsoleAction::CreateTask {
                acceptance,
                harness,
                skills,
                mode,
                repos,
                project,
                milestone,
                assignee,
                ..
            } => {
                assert!(acceptance.is_empty());
                assert!(harness.is_none());
                assert!(skills.is_empty());
                assert!(mode.is_none());
                assert!(repos.is_empty());
                assert!(project.is_none());
                assert!(milestone.is_none());
                assert!(assignee.is_none());
            }
            other => panic!("expected create_task, got {other:?}"),
        }
    }

    /// `actions` が無い・JSON でない・配列でないときは空（malformed も無し）。
    #[test]
    fn a_missing_actions_field_is_empty() {
        assert_eq!(
            actions_from_result_json(r#"{"summary":"s"}"#),
            ParsedActions::default()
        );
        assert_eq!(
            actions_from_result_json("not json"),
            ParsedActions::default()
        );
        assert_eq!(
            actions_from_result_json(r#"{"actions":"nope"}"#),
            ParsedActions::default()
        );
        assert!(ParsedActions::default().is_empty());
    }

    /// 形が合わない要素（知らない `type`、必須フィールド欠落）は実行できる要素とは別に、理由付きで残る。
    #[test]
    fn malformed_actions_are_kept_separately_with_a_reason() {
        let parsed = actions_from_result_json(
            r#"{"actions":[
                {"type":"create_task","title":"t"},
                {"type":"unknown_action"},
                {"type":"ask_human","text":"聞きたい"}
            ]}"#,
        );
        assert_eq!(parsed.valid.len(), 1, "{:?}", parsed.valid);
        assert_eq!(parsed.valid[0].kind(), "ask_human");
        assert_eq!(parsed.malformed.len(), 2, "{:?}", parsed.malformed);
        assert!(parsed.malformed[0].starts_with("action #1:"));
        assert!(parsed.malformed[1].starts_with("action #2:"));
    }

    /// 成果物ディレクトリが無い / `result.json` が無ければ空。
    #[test]
    fn a_missing_result_file_has_no_actions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifacts = dir.path().join(".taskd/artifacts/01HTASK");
        assert_eq!(read_result_actions(&artifacts), ParsedActions::default());
        std::fs::create_dir_all(&artifacts).expect("mkdir");
        std::fs::write(
            artifacts.join("result.json"),
            r#"{"actions":[{"type":"ask_human","text":"聞きたい"}]}"#,
        )
        .expect("write");
        let parsed = read_result_actions(&artifacts);
        assert_eq!(parsed.valid.len(), 1);
    }
}
