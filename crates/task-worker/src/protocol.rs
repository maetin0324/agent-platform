//! ワーカープロトコル v1 の型（DESIGN §5.3, ADR-0003 D2, `docs/protocol/worker-protocol.md`）。
//! JSON Schema は `schemars` で生成し `docs/protocol/worker-protocol.schema.json` と
//! テストで一致を検証する（ADR-0003 D6）。

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use task_core::{ArtifactRef, DelegateTask, GenreSpec, Status, Task, TaskId, Usage};

/// `run.protocol`。v2（ADR-0016 M9）: `delegate` メッセージ、`context.role`、`context.children`、`task.role` / `task.aggregate` を追加。
/// v3（ADR-0027 D1）: `context.available_genres`、`task.genre`、`delegate` の `tasks[].genre` を追加。
/// 全て追加のみで v1/v2 のワーカーはそのまま動く。
pub const PROTOCOL_VERSION: u32 = 3;

/// 直前のレビュー結果（`context.prior_review[]`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PriorReview {
    pub criterion: usize,
    pub pass: bool,
    pub reason: String,
}

/// `Reviewer` check のための `context.review`（ADR-0007 D5）。`RunRequest.task` は合成した `Review` kind の
/// タスクで、対象タスクの `run` の `done` の内容と、判定すべき条件のインデックスを渡す。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewRequest {
    /// 対象 run の `done.summary`。
    pub summary: String,
    /// 対象 run の `done.evidence`。
    pub evidence: Vec<Evidence>,
    /// 判定すべき `task.acceptance` のインデックス（`Check::Reviewer` の条件）。
    pub criteria: Vec<usize>,
}

/// 以前の `question` への人間の回答（`context.answers[]`。ADR-0010 D3, P-10）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Answer {
    pub question: String,
    pub answer: String,
}

/// `context.role`（ADR-0016 D1 / M3）: タスクの役割と `[[roles]]` の指示文。アダプタはプロンプトの前置きにする。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoleContext {
    pub id: String,
    /// 設定に指示文が無ければ空。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instructions: String,
}

/// `context.available_genres[].roles[]`（ADR-0027 D1）: 分野に属する役割の id。指示文までは渡さない
/// （長くなりすぎるため。プロンプトに前置きされる指示文は `context.role` 側の役目）ので `description` は
/// 今のところ常に省略だが、将来役割に説明文を持たせたときのために型としては残す。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GenreRoleContext {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `context.available_genres[]`（ADR-0027 D1）: 委譲できる run に渡す、使える分野と役割の一覧。
/// 「タスクの分野」ではなく「この run が子に割り当てられる分野の選択肢」を表す。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GenreContext {
    pub id: String,
    pub description: String,
    /// ADR-0028 D1/D2: この分野で「できること」の自由記述。空なら省略される。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// ADR-0028 D1/D2: この分野に渡すもの（目安、自由記述）。空なら省略される。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_artifacts: Vec<String>,
    /// ADR-0028 D1/D2: この分野から返るもの（目安、自由記述）。空なら省略される。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_artifacts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<GenreRoleContext>,
}

impl From<&GenreSpec> for GenreContext {
    fn from(g: &GenreSpec) -> Self {
        Self {
            id: g.id.clone(),
            description: g.description.clone(),
            capabilities: g.capabilities.clone(),
            input_artifacts: g.input_artifacts.clone(),
            output_artifacts: g.output_artifacts.clone(),
            roles: g.roles.iter().map(|id| GenreRoleContext { id: id.clone(), description: None }).collect(),
        }
    }
}

/// `context.children[]`（ADR-0016 D3 / M4）: 集約 run に渡す、委譲した子の要約。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChildSummary {
    pub id: TaskId,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub status: Status,
    /// 直近のワーカー run の `WorkerFinished.outcome`（無ければ省略）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// 子のワークスペースに残った成果物（`ArtifactProduced` の一覧。パスは子のワークスペース相対）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactRef>,
    /// 子のワークスペース（絶対パス。集約 run が成果物を読むため）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
}

/// `run.context`。未知フィールドは無視する（前方互換）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunContext {
    pub prior_review: Vec<PriorReview>,
    pub inputs: Vec<ArtifactRef>,
    /// `taskctl answer` で与えられた回答の履歴（時系列）。無ければ省略（ADR-0010 D3）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<Answer>,
    /// `Reviewer` check の run でのみ `Some`（ADR-0007 D5）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewRequest>,
    /// タスクに役割があるときだけ `Some`（ADR-0016 D1）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<RoleContext>,
    /// 集約 run（`aggregate = true` の親の、子が全て終端になった後の run）でのみ非空（ADR-0016 D3）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<ChildSummary>,
    /// ADR-0027 D1: 委譲できる run（`build_execute_prompt` を使う run）にだけ、設定済みの分野と
    /// その役割の一覧を渡す。委譲できない run（`Plan`/`Review`）や `[[genres]]` が空の設定では空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_genres: Vec<GenreContext>,
}

/// `error.provider_failure`（任意）: 供給側の失敗の種別（ADR-0010 D5, P-21）。付いていればディスパッチャは
/// attempts を消費せず `requeue` し、プロバイダを cooldown にする。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderFailure {
    Throttled { retry_after_secs: u64 },
    AuthFailed,
    Exhausted,
}

/// `artifacts/review.json` の 1 判定（ADR-0007 D1/D5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewVerdictOut {
    pub criterion: usize,
    pub pass: bool,
    pub reason: String,
}

/// `Review` run がワークスペース直下 `artifacts/review.json` に書く出力（ADR-0007 D1/D5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewOutput {
    pub verdicts: Vec<ReviewVerdictOut>,
}

/// taskd → ワーカーの `run` メッセージ（1 行）。`{"type":"run", ...}`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename = "run")]
pub struct RunRequest {
    pub protocol: u32,
    pub task: Task,
    /// 絶対パス。ワーカーの cwd、`artifact.path` の基準。
    pub workspace: PathBuf,
    pub context: RunContext,
}

/// `done.evidence[]`。`command` / `exit` / `stdout_tail` は、コマンドを伴わない条件（`ArtifactExists` / `Reviewer` / `Human`）では
/// 存在しないので任意（ADR-0012 D3, P-12。必須 → 任意の緩和なので後方互換）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Evidence {
    pub criterion: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_tail: Option<String>,
}

/// ワーカー → taskd のメッセージ。`done` / `error` / `question` は終端（ADR-0003 D3）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerMessage {
    Progress {
        msg: String,
    },
    /// ADR-0016 D2: 実行中の委譲の提案（任意回、非終端）。taskd は検証を通ったものだけ子タスクとして挿入し、
    /// 拒否した提案は理由を `WorkerProgress` に残す。run は失敗しない。
    Delegate {
        tasks: Vec<DelegateTask>,
    },
    Artifact {
        name: String,
        /// ワークスペース相対。絶対パス・`..`・ワークスペース外は拒否（ADR-0003 D5）。
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
    },
    Question {
        text: String,
    },
    Done {
        summary: String,
        evidence: Vec<Evidence>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
    },
    Error {
        message: String,
        retryable: bool,
        /// 供給側の失敗なら種別を付ける（ADR-0010 D5）。付いていれば `retryable` に関わらず `requeue` になる。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_failure: Option<ProviderFailure>,
    },
}

impl WorkerMessage {
    /// 終端メッセージか（ADR-0003 D3）。
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            WorkerMessage::Question { .. } | WorkerMessage::Done { .. } | WorkerMessage::Error { .. }
        )
    }
}

/// スキーマ生成用のルート。`docs/protocol/worker-protocol.schema.json` の内容と一致する。
#[derive(Debug, JsonSchema)]
#[allow(dead_code)]
pub struct ProtocolSchema {
    pub run: RunRequest,
    pub message: WorkerMessage,
    /// `artifacts/review.json`（ADR-0007 D1）。
    pub review_output: ReviewOutput,
}

/// 生成したスキーマ（`serde_json::Value`）。
pub fn schema_value() -> serde_json::Value {
    let schema = schemars::schema_for!(ProtocolSchema);
    serde_json::to_value(schema).unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn worker_message_roundtrip_and_unknown_fields_ignored() {
        let line = r#"{"type":"done","summary":"s","evidence":[{"criterion":0,"command":"true","exit":0,"stdout_tail":""}],"extra":1}"#;
        let m: WorkerMessage = serde_json::from_str(line).unwrap();
        assert!(m.is_terminal());
        match &m {
            WorkerMessage::Done { summary, evidence, usage } => {
                assert_eq!(summary, "s");
                assert_eq!(evidence.len(), 1);
                assert!(usage.is_none());
            }
            _ => panic!("expected done"),
        }
        let back = serde_json::to_string(&m).unwrap();
        assert!(back.starts_with(r#"{"type":"done""#));
        assert!(!back.contains("usage"));
    }

    #[test]
    fn unknown_type_is_a_parse_error_and_missing_required_is_error() {
        assert!(serde_json::from_str::<WorkerMessage>(r#"{"type":"bogus"}"#).is_err());
        assert!(serde_json::from_str::<WorkerMessage>(r#"{"type":"error","message":"m"}"#).is_err());
        assert!(!serde_json::from_str::<WorkerMessage>(r#"{"type":"progress","msg":"m"}"#).unwrap().is_terminal());
    }

    /// ADR-0012 D3（P-12）: コマンドを伴わない条件の evidence は `criterion` だけでよく、旧形式（全フィールドあり）も読める。
    #[test]
    fn evidence_fields_other_than_criterion_are_optional() {
        let line = r#"{"type":"done","summary":"s","evidence":[{"criterion":1},{"criterion":0,"command":"cargo test","exit":0,"stdout_tail":"ok"}]}"#;
        let WorkerMessage::Done { evidence, .. } = serde_json::from_str::<WorkerMessage>(line).unwrap() else {
            panic!("expected done");
        };
        assert_eq!(evidence[0], Evidence { criterion: 1, command: None, exit: None, stdout_tail: None });
        assert_eq!(evidence[1].command.as_deref(), Some("cargo test"));
        assert_eq!(evidence[1].exit, Some(0));
        assert_eq!(serde_json::to_string(&evidence[0]).unwrap(), r#"{"criterion":1}"#);
    }

    /// ADR-0016 D2: `delegate` は非終端で、`tasks` は `DelegateTask`。`depends_on` は整数と ID 文字列を混ぜられる。
    #[test]
    fn delegate_message_parses_and_is_not_terminal() {
        let line = r#"{"type":"delegate","tasks":[{"title":"a","objective":"o","acceptance":[{"text":"c","check":{"type":"human"}}],"role":"implementer","genre":"coding"},
            {"title":"b","objective":"o","acceptance":[{"text":"c","check":{"type":"command","cmd":"true","expect_exit":0}}],"depends_on":[0]}]}"#;
        let m: WorkerMessage = serde_json::from_str(line).unwrap();
        assert!(!m.is_terminal());
        let WorkerMessage::Delegate { tasks } = m else { panic!("expected delegate") };
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].role.as_deref(), Some("implementer"));
        // ADR-0027 D1: `genre` は任意。
        assert_eq!(tasks[0].genre.as_deref(), Some("coding"));
        assert_eq!(tasks[1].genre, None);
        assert_eq!(tasks[1].depends_on, vec![task_core::DelegateDep::Index(0)]);
    }

    #[test]
    fn run_request_serializes_with_type_tag() {
        let req = RunRequest {
            protocol: PROTOCOL_VERSION,
            task: sample_task(),
            workspace: PathBuf::from("/tmp/ws"),
            context: RunContext::default(),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["type"], "run");
        assert_eq!(v["protocol"], 3);
        assert_eq!(v["task"]["kind"], "execute");
        let back: RunRequest = serde_json::from_value(v).unwrap();
        assert_eq!(back, req);
    }

    /// ADR-0027 D1: `available_genres` は空なら省略され、非空なら分野と役割の一覧が乗る。
    #[test]
    fn available_genres_round_trips_and_is_omitted_when_empty() {
        let empty = RunContext::default();
        let v = serde_json::to_value(&empty).unwrap();
        assert!(v.get("available_genres").is_none());

        let genres = [task_core::GenreSpec {
            id: "literature".into(),
            description: "related work survey".into(),
            capabilities: vec!["academic literature search".into(), "citation graph traversal".into()],
            input_artifacts: vec!["question".into(), "pdf".into()],
            output_artifacts: vec!["answer.md".into(), "citations.json".into()],
            default_role: Some("literature-reader".into()),
            roles: vec!["literature-scout".into(), "literature-reader".into()],
        }];
        let context = RunContext {
            available_genres: genres.iter().map(GenreContext::from).collect(),
            ..RunContext::default()
        };
        let v = serde_json::to_value(&context).unwrap();
        assert_eq!(v["available_genres"][0]["id"], "literature");
        assert_eq!(v["available_genres"][0]["roles"][1]["id"], "literature-reader");
        assert_eq!(v["available_genres"][0]["capabilities"][1], "citation graph traversal");
        assert_eq!(v["available_genres"][0]["input_artifacts"], serde_json::json!(["question", "pdf"]));
        assert_eq!(v["available_genres"][0]["output_artifacts"], serde_json::json!(["answer.md", "citations.json"]));
        let back: RunContext = serde_json::from_value(v).unwrap();
        assert_eq!(back, context);

        // 空なら 3 フィールドとも省略される（既存設定との互換）。
        let bare_genre = task_core::GenreSpec {
            id: "coding".into(),
            description: "write and fix code".into(),
            ..task_core::GenreSpec::default()
        };
        let bare_json = serde_json::to_value(GenreContext::from(&bare_genre)).unwrap();
        assert!(bare_json.get("capabilities").is_none());
        assert!(bare_json.get("input_artifacts").is_none());
        assert!(bare_json.get("output_artifacts").is_none());
    }

    /// ADR-0003 D6: 生成スキーマとコミット済みファイルの一致。`UPDATE_SCHEMA=1` で再生成する。
    #[test]
    fn committed_schema_matches_generated() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/protocol/worker-protocol.schema.json");
        let generated = serde_json::to_string_pretty(&schema_value()).unwrap() + "\n";
        if std::env::var_os("UPDATE_SCHEMA").is_some() {
            std::fs::write(path, &generated).unwrap();
        }
        let committed = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {path}: {e} (run with UPDATE_SCHEMA=1 to generate)"));
        assert_eq!(committed, generated, "schema drift: run `UPDATE_SCHEMA=1 cargo test -p task-worker`");
    }

    pub(crate) fn sample_task() -> Task {
        use task_core::*;
        let now = time::OffsetDateTime::now_utc();
        Task {
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "t".into(),
            objective: "o".into(),
            acceptance: vec![Criterion { text: "c".into(), check: Check::Command { cmd: "true".into(), expect_exit: 0 } }],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Running,
            priority: 0,
            worker_hint: WorkerHint { tier: Tier::Standard, adapter: None },
            workspace: WorkspaceSpec::Local { path: PathBuf::from("/tmp/ws") },
            budget: Budget { max_turns: 10, max_wall_secs: 60, max_retries: 1 },
            attempts: 0,
            lease: None,
            created_at: now,
            updated_at: now,
            role: None,
            genre: None,
            aggregate: false,
        }
    }
}
