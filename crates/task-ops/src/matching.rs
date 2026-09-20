//! 担当の選び方は決定的（ADR-0046 D5 の capability matching）。
//!
//! `assignee` が無いタスク（計画 run の子、Console から作られたタスク、人が作ったタスク）の担当を、
//! **LLM を使わずに**決める。ディスパッチャの前段で 1 回だけ走る（DESIGN 原則 1: 判断は決定的に）。
//!
//! 規則（D5 そのまま）:
//! - 候補 = 実効 profile の `harnesses.allowed` にそのタスクの harness を含む**葉と中間のノード全部**
//!   （根は除く）。
//! - スコア = |タスクの skills ∩ ノードの実効 skills|。最大スコアのノード。
//! - 同点は**浅い方**、さらに同点は id の辞書順。
//! - タスクの skills が空なら「その harness を `default` に持つノード」を優先し、無ければ allowed を
//!   持つ最も浅いノード。
//! - 候補が無ければ `blocked` にして人に聞く（ADR-0021 の質問経路）。

use task_core::{OrgKind, OrgNode, Task, TaskStore};

use crate::error::OpsError;

/// `assign` の結果（ADR-0046 D5）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Assignment {
    /// 担当が決まった。
    Assigned {
        node: String,
        score: usize,
        reason: String,
    },
    /// 候補が 1 つも無い。呼び出し側は `blocked` にして人に聞く（ADR-0021 の質問経路）。
    Unroutable { question: String },
    /// matching を走らせる対象ではない（既に担当が居る、またはハーネスが決まっていない）。
    /// **何もしない**（Phase 59 より前のタスクの挙動を変えない）。
    NotApplicable,
}

/// ADR-0046 D5: 候補が無いときに人へ出す質問（決定的な文面）。
pub fn unroutable_question(harness: &str, skills: &[String]) -> String {
    let skills = if skills.is_empty() {
        "（指定なし）".to_string()
    } else {
        skills.join(", ")
    };
    format!(
        "担当が見つからない: harness {harness} / skills {skills}。\
         この仕事を受けられる組織のノードが 1 つもありません。\
         そのハーネスを `harnesses.allowed` に持つノードを作る（または既存のノードに足す）か、\
         このタスクの担当を直接指定してください。"
    )
}

/// ストアの組織図を読んで担当を決める（ADR-0046 D5）。判断そのものは [`decide`]（純粋関数）。
pub fn assign(store: &dyn TaskStore, task: &Task) -> Result<Assignment, OpsError> {
    let org = store.org_list()?;
    Ok(decide(&org, task))
}

/// ADR-0046 D5 の決定的な判断（純粋関数。テスト容易）。
pub fn decide(org: &[OrgNode], task: &Task) -> Assignment {
    if task.assignee.is_some() {
        return Assignment::NotApplicable;
    }
    // ハーネスが決まっていないタスクは matching の対象外（従来どおり担当なしで走る）。
    let Some(harness) = task.genre.as_deref().filter(|g| !g.is_empty()) else {
        return Assignment::NotApplicable;
    };
    // ADR-0046 D5 / Phase 59 の規律「Phase 59 より前の組織を壊さない」: 組織を 1 つも作っていない
    // 構成（`org_include` を書いていない・`org_nodes` が空）では matching そのものを走らせない
    // （さもないと ADR-0041 D5 の `smoke` 煙試験のような、組織を使わない既存の genre 付きタスクが
    // 軒並み `blocked` になってしまう）。
    if org.is_empty() {
        return Assignment::NotApplicable;
    }

    // 候補: 根を除く全ノードのうち、実効 profile がその harness を許すもの。
    let mut candidates: Vec<Candidate> = Vec::new();
    for node in org {
        if is_root(org, node) {
            continue;
        }
        let effective = task_core::resolve_profile(org, &node.id);
        if !effective.allows_harness(harness) {
            continue;
        }
        let score = task
            .skills
            .iter()
            .filter(|s| effective.skills.iter().any(|have| have == *s))
            .count();
        candidates.push(Candidate {
            id: node.id.clone(),
            depth: effective.chain.len(),
            score,
            is_default: effective.harness_default.as_deref() == Some(harness),
            matched: task
                .skills
                .iter()
                .filter(|s| effective.skills.iter().any(|have| have == *s))
                .cloned()
                .collect(),
        });
    }
    if candidates.is_empty() {
        return Assignment::Unroutable {
            question: unroutable_question(harness, &task.skills),
        };
    }

    // タスクの skills が空なら「その harness を `default` に持つノード」を優先する。
    if task.skills.is_empty() && candidates.iter().any(|c| c.is_default) {
        candidates.retain(|c| c.is_default);
    }
    // 最大スコア → 浅い方 → id の辞書順（全部決定的）。
    candidates.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.depth.cmp(&b.depth))
            .then_with(|| a.id.cmp(&b.id))
    });
    let best = &candidates[0];
    let reason = if task.skills.is_empty() {
        if best.is_default {
            format!("harness {harness} を既定に持つ最も浅いノード")
        } else {
            format!("harness {harness} を受けられる最も浅いノード")
        }
    } else if best.score == 0 {
        format!("harness {harness} を受けられる最も浅いノード（skill の重なりは無し）")
    } else {
        format!(
            "harness {harness} / skill の重なり {} 件（{}）",
            best.score,
            best.matched.join(", ")
        )
    };
    Assignment::Assigned {
        node: best.id.clone(),
        score: best.score,
        reason,
    }
}

struct Candidate {
    id: String,
    depth: usize,
    score: usize,
    is_default: bool,
    matched: Vec<String>,
}

/// 根のノードか（CoS。`kind = secretary` か、親を持たないノード）。
fn is_root(org: &[OrgNode], node: &OrgNode) -> bool {
    let _ = org;
    node.kind == OrgKind::Secretary || node.parent_id.is_none()
}

/// ADR-0046 D5 / D3: 明示の `assignee` がそのハーネスを受けられるか。ノードが `harnesses.allowed` を
/// 1 つも持たない（profile を書いていない）ときは**従来どおり通す**（Phase 59 より前の組織を壊さない）。
pub fn assignee_accepts(
    org: &[OrgNode],
    assignee: &str,
    harness: Option<&str>,
) -> Result<(), String> {
    let Some(harness) = harness.filter(|h| !h.is_empty()) else {
        return Ok(());
    };
    let effective = task_core::resolve_profile(org, assignee);
    if effective.harnesses_allowed.is_empty() || effective.allows_harness(harness) {
        return Ok(());
    }
    Err(format!(
        "assignee {assignee:?} cannot take harness {harness:?} (allowed: {})",
        effective.harnesses_allowed.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_core::{HarnessPrefs, Profile};
    use time::OffsetDateTime;

    fn node(
        id: &str,
        parent: Option<&str>,
        allowed: &[&str],
        default: Option<&str>,
        skills: &[&str],
    ) -> OrgNode {
        let now = OffsetDateTime::now_utc();
        OrgNode {
            id: id.into(),
            parent_id: parent.map(str::to_string),
            name: id.into(),
            kind: if parent.is_none() {
                OrgKind::Secretary
            } else {
                OrgKind::Department
            },
            genre: None,
            brief: String::new(),
            profile: Profile {
                skills: skills.iter().map(|s| s.to_string()).collect(),
                harnesses: HarnessPrefs {
                    allowed: allowed.iter().map(|s| s.to_string()).collect(),
                    default: default.map(str::to_string),
                },
                ..Profile::default()
            },
            position: 0,
            created_at: now,
            updated_at: now,
        }
    }

    fn org() -> Vec<OrgNode> {
        vec![
            node(
                "cos",
                None,
                &["conversation", "plan"],
                Some("conversation"),
                &[],
            ),
            node("engineering", Some("cos"), &["coding"], None, &["software"]),
            node(
                "software-engineering",
                Some("engineering"),
                &[],
                Some("coding"),
                &["rust", "sqlite"],
            ),
            node(
                "systems-performance",
                Some("engineering"),
                &["data-analysis"],
                None,
                &["hpc", "benchmark", "rust"],
            ),
        ]
    }

    fn task(harness: Option<&str>, skills: &[&str]) -> Task {
        let mut t = sample_task();
        t.genre = harness.map(str::to_string);
        t.skills = skills.iter().map(|s| s.to_string()).collect();
        t
    }

    fn sample_task() -> Task {
        use task_core::{
            Budget, Status, TaskId, TaskKind, TaskMode, Tier, WorkerHint, WorkspaceSpec,
        };
        let now = OffsetDateTime::now_utc();
        Task {
            id: TaskId::new(),
            parent_id: None,
            kind: TaskKind::Execute,
            title: "t".into(),
            objective: "o".into(),
            acceptance: vec![],
            inputs: vec![],
            depends_on: vec![],
            status: Status::Ready,
            priority: 10,
            worker_hint: WorkerHint {
                tier: Tier::Standard,
                adapter: None,
            },
            workspace: WorkspaceSpec::local("/tmp"),
            repos: vec![],
            budget: Budget {
                max_turns: 1,
                max_wall_secs: 1,
                max_retries: 0,
            },
            attempts: 0,
            lease: None,
            created_at: now,
            updated_at: now,
            role: None,
            genre: None,
            aggregate: false,
            project_id: None,
            milestone_id: None,
            assignee: None,
            labels: vec![],
            category: Default::default(),
            skills: vec![],
            mode: TaskMode::Production,
            conversation: None,
        }
    }

    /// ADR-0046 §4-4: スコアは skill の重なり。最大スコアが勝つ。
    #[test]
    fn the_node_with_the_most_overlapping_skills_wins() {
        let org = org();
        let assignment = decide(&org, &task(Some("coding"), &["hpc", "benchmark"]));
        assert_eq!(
            assignment,
            Assignment::Assigned {
                node: "systems-performance".into(),
                score: 2,
                reason: "harness coding / skill の重なり 2 件（hpc, benchmark）".into(),
            }
        );
        // rust は両方が持つが、software-engineering の方が sqlite も持つ。
        let assignment = decide(&org, &task(Some("coding"), &["rust", "sqlite"]));
        let Assignment::Assigned { node, score, .. } = assignment else {
            panic!("assigned");
        };
        assert_eq!((node.as_str(), score), ("software-engineering", 2));
    }

    /// 同点は浅い方、さらに同点は id の辞書順。
    #[test]
    fn ties_go_to_the_shallower_node_then_to_the_lexicographically_smaller_id() {
        let org = org();
        // `rust` は engineering の子 2 つが両方持つ（スコア 1）。深さは同じなので id 順。
        let Assignment::Assigned { node, .. } = decide(&org, &task(Some("coding"), &["rust"]))
        else {
            panic!("assigned");
        };
        assert_eq!(node, "software-engineering");
        // `software` は engineering（深さ 2）だけが持つ。
        let Assignment::Assigned { node, score, .. } =
            decide(&org, &task(Some("coding"), &["software"]))
        else {
            panic!("assigned");
        };
        assert_eq!((node.as_str(), score), ("engineering", 1));
    }

    /// skills が空なら、その harness を `default` に持つノードを優先する。
    #[test]
    fn without_skills_the_default_harness_node_wins_then_the_shallowest() {
        let org = org();
        let Assignment::Assigned {
            node,
            score,
            reason,
        } = decide(&org, &task(Some("coding"), &[]))
        else {
            panic!("assigned");
        };
        assert_eq!((node.as_str(), score), ("software-engineering", 0));
        assert_eq!(reason, "harness coding を既定に持つ最も浅いノード");
        // `data-analysis` を default に持つノードは無いので、allowed を持つ最も浅いノード。
        let Assignment::Assigned { node, .. } = decide(&org, &task(Some("data-analysis"), &[]))
        else {
            panic!("assigned");
        };
        assert_eq!(node, "systems-performance");
    }

    /// 根（CoS）は候補に入らない。`harnesses.allowed` は親と和なので、根だけに書いた harness
    /// （`conversation`）も子は実効的に継ぐ——それでも根自身が担当に選ばれることは無い。
    #[test]
    fn the_root_is_never_a_candidate() {
        let org = org();
        let Assignment::Assigned { node: assigned, .. } =
            decide(&org, &task(Some("conversation"), &[]))
        else {
            panic!("engineering 以下が継いでいるので Assigned のはず");
        };
        assert_ne!(assigned, "cos");
        assert_eq!(assigned, "engineering", "根の直下でいちばん浅い");

        // 根しか無い組織では、根だけが持つ harness は誰にも継がれず候補が無い。
        let root_only = vec![node(
            "cos",
            None,
            &["conversation"],
            Some("conversation"),
            &[],
        )];
        assert!(matches!(
            decide(&root_only, &task(Some("conversation"), &[])),
            Assignment::Unroutable { .. }
        ));
    }

    /// 候補が無ければ `Unroutable`（呼び出し側が `blocked` にして人に聞く）。
    #[test]
    fn no_candidate_is_unroutable_with_a_question() {
        let org = org();
        let Assignment::Unroutable { question } =
            decide(&org, &task(Some("literature"), &["paper-writing"]))
        else {
            panic!("unroutable");
        };
        assert!(
            question.starts_with("担当が見つからない: harness literature / skills paper-writing"),
            "{question}"
        );
    }

    /// 既に担当が居る・ハーネスが無いタスクは対象外（何もしない）。
    #[test]
    fn a_task_with_an_assignee_or_without_a_harness_is_not_applicable() {
        let org = org();
        let mut t = task(Some("coding"), &["rust"]);
        t.assignee = Some("engineering".into());
        assert_eq!(decide(&org, &t), Assignment::NotApplicable);
        assert_eq!(
            decide(&org, &task(None, &["rust"])),
            Assignment::NotApplicable
        );
        assert_eq!(
            decide(&org, &task(Some(""), &[])),
            Assignment::NotApplicable
        );
    }

    /// Phase 59 より前の組織（`org_nodes` が空。組織そのものを使っていない構成）では、genre 付きの
    /// タスクでも matching を走らせない（さもないと ADR-0041 D5 の `smoke` 煙試験のような、組織を
    /// 使わない既存のタスクが軒並み `blocked` になってしまう）。
    #[test]
    fn an_empty_org_never_runs_matching() {
        assert_eq!(
            decide(&[], &task(Some("smoke"), &[])),
            Assignment::NotApplicable
        );
        assert_eq!(
            decide(&[], &task(Some("coding"), &["rust"])),
            Assignment::NotApplicable
        );
    }

    /// ADR-0046 D5: 明示の `assignee` が受けられないハーネスは弾く（API は 422）。
    #[test]
    fn an_explicit_assignee_must_allow_the_harness() {
        let org = org();
        assignee_accepts(&org, "software-engineering", Some("coding")).expect("allowed");
        let err = assignee_accepts(&org, "software-engineering", Some("literature"))
            .expect_err("rejected");
        assert!(err.contains("cannot take harness"), "{err}");
        // ハーネスを書かないタスクは通す。
        assignee_accepts(&org, "software-engineering", None).expect("no harness");
        // profile を持たないノード（Phase 59 より前の組織）は従来どおり通す。
        let bare = vec![node("infra", Some("cos"), &[], None, &[])];
        assignee_accepts(&bare, "infra", Some("coding")).expect("no profile: allowed");
        // 知らないノードも通す（存在の検査は呼び出し側）。
        assignee_accepts(&org, "ghost", Some("coding")).expect("unknown node");
    }
}
