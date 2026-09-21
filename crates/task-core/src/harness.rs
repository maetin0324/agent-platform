//! ハーネス = 実行契約（ADR-0046 D3）。純粋なデータ定義と決定的な変換だけを置く
//! （I/O・LLM 呼び出しはしない。ADR-0001 D2 / DESIGN 原則 1）。
//!
//! 今までの `[[genres]]`（能力・入出力の契約・対話用か）と `[[roles]]`（adapter・tier・指示文・予算）を
//! 1 つの `[[harnesses]]` に統合する。**組織と 1 対 1 にしない**（1 つのノードが複数のハーネスを
//! 受けられるし、同じハーネスを複数のノードが使う）。
//!
//! 互換（ADR-0046 D3「設定の互換」）: 旧い `[[genres]]` + `[[roles]]` は
//! [`HarnessRegistry::from_legacy`] が決定的に写す。逆に `[[harnesses]]` から
//! [`HarnessRegistry::genre_specs`] / [`HarnessRegistry::role_specs`] で旧い形に戻せるので、
//! ディスパッチャ・task-ops の既存経路は一切変えずに動く（Phase 59 の逸脱として `PROGRESS.md` に記録）。

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::{GenreSpec, RoleSpec, Tier};

/// ADR-0046 D3: 組み込みのハーネス id（設定が同じ id を書けば上書きされる）。
pub const BUILTIN_CONVERSATION: &str = "conversation";
pub const BUILTIN_PLAN: &str = "plan";
pub const BUILTIN_REVIEWER: &str = "reviewer";
pub const BUILTIN_SMOKE: &str = "smoke";
/// ADR-0047 D4（Phase 62）: 知識整理 run（LangMem による抽出）。
pub const BUILTIN_KNOWLEDGE: &str = "knowledge";

/// 組み込みのハーネス id の一覧（並びは決定的）。
pub const BUILTIN_HARNESSES: [&str; 5] = [
    BUILTIN_CONVERSATION,
    BUILTIN_PLAN,
    BUILTIN_REVIEWER,
    BUILTIN_SMOKE,
    BUILTIN_KNOWLEDGE,
];

/// ADR-0046 D3: ハーネスの予算。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HarnessBudget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wall_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
}

impl HarnessBudget {
    pub fn is_empty(&self) -> bool {
        self.max_turns.is_none() && self.max_wall_secs.is_none() && self.max_retries.is_none()
    }
}

/// ADR-0052 D2（Phase 64）: ハーネスの**フォールバック**（`[[harnesses]] fallback`）。
///
/// ```toml
/// fallback = { tier = "cheap" }   # 専用アダプタに届かないとき、この tier の汎用ハーネスへ倒す
/// fallback = false                # 倒さない（従来どおり失敗する）
/// ```
///
/// 今のところ読むのは `knowledge` ハーネス（ADR-0047 D4 の知識整理 run）だけ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum HarnessFallback {
    /// `fallback = false`（無効）/ `fallback = true`（既定の tier = `cheap`）。
    Switch(bool),
    /// `fallback = { tier = "cheap" }`。
    Tier(HarnessFallbackTier),
}

/// [`HarnessFallback::Tier`] の中身。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HarnessFallbackTier {
    pub tier: Tier,
}

/// `fallback = true` と書かれたときの tier（ADR-0052 D2 の既定）。
pub const DEFAULT_FALLBACK_TIER: Tier = Tier::Cheap;

impl HarnessFallback {
    /// 倒す先の tier（無効なら `None`）。
    pub fn tier(&self) -> Option<Tier> {
        match self {
            HarnessFallback::Switch(false) => None,
            HarnessFallback::Switch(true) => Some(DEFAULT_FALLBACK_TIER),
            HarnessFallback::Tier(t) => Some(t.tier),
        }
    }
}

/// ADR-0046 D3: ハーネス 1 件（`[[harnesses]]` の 1 行）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HarnessSpec {
    pub id: String,
    #[serde(default)]
    pub description: String,
    /// ワーカーのアダプタ id（`claude-code` / `codex` / `acp` / `paperqa` / `local-deep-research` / `fake`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    /// 既定の tier（ノード・タスクが上書きする）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<Tier>,
    /// 前置きに出す指示文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_artifacts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_artifacts: Vec<String>,
    #[serde(default, skip_serializing_if = "HarnessBudget::is_empty")]
    pub budget: HarnessBudget,
    /// 対話用のハーネスか（今までの「対話用分野」）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub conversation: bool,
    /// ADR-0052 D2（Phase 64）: 専用アダプタに届かないときに倒す先。`None` は「倒さない」。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<HarnessFallback>,
}

impl HarnessSpec {
    /// ADR-0052 D2: 倒す先の tier（`fallback` を書いていない・`fallback = false` なら `None`）。
    pub fn fallback_tier(&self) -> Option<Tier> {
        self.fallback.as_ref().and_then(HarnessFallback::tier)
    }

    /// 旧い `[[roles]]` の 1 行に戻す（既存の経路が使う互換の射影。`id` はハーネス id）。
    pub fn role_spec(&self) -> RoleSpec {
        RoleSpec {
            id: self.id.clone(),
            tier: self.tier,
            adapter: self.adapter.clone(),
            max_turns: self.budget.max_turns,
            max_wall_secs: self.budget.max_wall_secs,
            instructions: self.instructions.clone(),
        }
    }

    /// 旧い `[[genres]]` の 1 行に戻す（`default_role` は自分自身の id）。
    pub fn genre_spec(&self) -> GenreSpec {
        GenreSpec {
            id: self.id.clone(),
            description: self.description.clone(),
            capabilities: self.capabilities.clone(),
            input_artifacts: self.input_artifacts.clone(),
            output_artifacts: self.output_artifacts.clone(),
            default_role: Some(self.id.clone()),
            roles: vec![self.id.clone()],
        }
    }

    /// 旧い役割の値で埋める（`Some` だけが勝つ。ADR-0046 D3「leftover roles は id でハーネスを上書き」）。
    pub fn apply_role(&mut self, role: &RoleSpec) {
        if role.tier.is_some() {
            self.tier = role.tier;
        }
        if role.adapter.is_some() {
            self.adapter = role.adapter.clone();
        }
        if role.instructions.is_some() {
            self.instructions = role.instructions.clone();
        }
        if role.max_turns.is_some() {
            self.budget.max_turns = role.max_turns;
        }
        if role.max_wall_secs.is_some() {
            self.budget.max_wall_secs = role.max_wall_secs;
        }
    }
}

/// ADR-0046 D3: 組み込みのハーネス（設定が同じ id を書けば上書きされる）。
pub fn builtin_harnesses() -> Vec<HarnessSpec> {
    vec![
        HarnessSpec {
            id: BUILTIN_CONVERSATION.into(),
            description: "人と話し、案件を理解し、組織に流す".into(),
            tier: Some(Tier::Standard),
            conversation: true,
            input_artifacts: vec!["人の依頼文".into(), "下から上がった報告".into()],
            output_artifacts: vec!["返事（result.json の summary）".into()],
            ..HarnessSpec::default()
        },
        HarnessSpec {
            id: BUILTIN_PLAN.into(),
            description: "案件を分解し、子タスクの計画を書く".into(),
            tier: Some(Tier::Standard),
            input_artifacts: vec!["案件の依頼文".into(), "途中目標".into()],
            output_artifacts: vec!["plan.json".into()],
            ..HarnessSpec::default()
        },
        HarnessSpec {
            id: BUILTIN_REVIEWER.into(),
            description: "受け入れ条件を判定する".into(),
            tier: Some(Tier::Standard),
            input_artifacts: vec!["対象 run の summary と evidence".into()],
            output_artifacts: vec!["review.json".into()],
            ..HarnessSpec::default()
        },
        HarnessSpec {
            id: BUILTIN_SMOKE.into(),
            description: "検証の煙試験".into(),
            adapter: Some("fake".into()),
            tier: Some(Tier::Standard),
            budget: HarnessBudget {
                max_turns: Some(1),
                max_wall_secs: Some(60),
                max_retries: None,
            },
            ..HarnessSpec::default()
        },
        HarnessSpec {
            id: BUILTIN_KNOWLEDGE.into(),
            description: "終端になったタスクから知識の候補を抽出し整理する（ADR-0047 D4）".into(),
            adapter: Some("langmem".into()),
            tier: Some(Tier::Cheap),
            input_artifacts: vec!["報告".into(), "関連する知識ベースのページ".into()],
            output_artifacts: vec!["artifacts/knowledge-candidates.json".into()],
            budget: HarnessBudget {
                max_turns: Some(4),
                max_wall_secs: Some(900),
                max_retries: Some(1),
            },
            // ADR-0052 D2: Qwen（`langmem`）に届かなければ tier `cheap` の汎用ハーネスへ倒す。
            fallback: Some(HarnessFallback::Tier(HarnessFallbackTier {
                tier: Tier::Cheap,
            })),
            ..HarnessSpec::default()
        },
    ]
}

/// ADR-0046 D3: ハーネスのレジストリ。`get(id)` が唯一の引き方。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HarnessRegistry {
    harnesses: Vec<HarnessSpec>,
}

impl HarnessRegistry {
    /// 設定に書かれたハーネスから作る。組み込みのうち**設定に無い id** だけを後ろに足す
    /// （設定が組み込みを上書きできる。ADR-0046 D3）。
    pub fn new(declared: Vec<HarnessSpec>) -> Self {
        let mut harnesses = declared;
        for builtin in builtin_harnesses() {
            match harnesses.iter_mut().find(|h| h.id == builtin.id) {
                // ADR-0052 D2: `fallback` は**組み込みの既定**。同じ id を設定に書いただけで黙って
                // 無効にならないよう、設定が `fallback` を書いていなければ組み込みの値を継ぐ
                // （消したいときは `fallback = false` と明示する）。
                Some(declared) => {
                    if declared.fallback.is_none() {
                        declared.fallback = builtin.fallback.clone();
                    }
                }
                None => harnesses.push(builtin),
            }
        }
        Self { harnesses }
    }

    /// 組み込みだけのレジストリ（`[[harnesses]]` も `[[genres]]` も無い最小構成）。
    pub fn builtins_only() -> Self {
        Self::new(Vec::new())
    }

    pub fn get(&self, id: &str) -> Option<&HarnessSpec> {
        self.harnesses.iter().find(|h| h.id == id)
    }

    pub fn all(&self) -> &[HarnessSpec] {
        &self.harnesses
    }

    pub fn ids(&self) -> Vec<String> {
        self.harnesses.iter().map(|h| h.id.clone()).collect()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.get(id).is_some()
    }

    /// 対話用のハーネス（`conversation = true`）の id（最初の 1 件）。
    pub fn conversation_id(&self) -> Option<&str> {
        self.harnesses
            .iter()
            .find(|h| h.conversation)
            .map(|h| h.id.as_str())
    }

    /// 互換の射影: 旧い `[[genres]]`。
    pub fn genre_specs(&self) -> Vec<GenreSpec> {
        self.harnesses.iter().map(HarnessSpec::genre_spec).collect()
    }

    /// 互換の射影: 旧い `[[roles]]`。
    pub fn role_specs(&self) -> Vec<RoleSpec> {
        self.harnesses.iter().map(HarnessSpec::role_spec).collect()
    }

    /// ADR-0046 D3「設定の互換」: 旧い `[[genres]]` + `[[roles]]` を決定的に写す。
    ///
    /// - genre.id をそのままハーネス id にする。`default_role` の役割が adapter / tier / 指示文 / 予算を与える。
    /// - `conversation_genre` と同じ id の分野は `conversation = true` にする。
    /// - どの分野の `default_role` でもない役割は「**id が一致するハーネスの上書き**」として扱う
    ///   （`reviewer` は組み込みの `reviewer` を、対話用の役割は対話用のハーネスを上書きする）。
    ///   一致するハーネスが無い役割は写さない（`tasks.role` は読み取り専用の互換として残るだけ）。
    ///
    /// 戻り値の 2 つめは「写さなかった役割の id」（呼び出し側が warn に出す）。
    pub fn from_legacy(
        genres: &[GenreSpec],
        roles: &[RoleSpec],
        conversation_genre: &str,
    ) -> (Self, Vec<String>) {
        let mut declared: Vec<HarnessSpec> = Vec::with_capacity(genres.len());
        for g in genres {
            let mut spec = HarnessSpec {
                id: g.id.clone(),
                description: g.description.clone(),
                capabilities: g.capabilities.clone(),
                input_artifacts: g.input_artifacts.clone(),
                output_artifacts: g.output_artifacts.clone(),
                conversation: g.id == conversation_genre,
                ..HarnessSpec::default()
            };
            if let Some(default_role) = g.default_role.as_deref()
                && let Some(role) = RoleSpec::find(roles, default_role)
            {
                spec.apply_role(role);
            }
            declared.push(spec);
        }
        let mut registry = Self::new(declared);
        let used: Vec<&str> = genres
            .iter()
            .filter_map(|g| g.default_role.as_deref())
            .collect();
        let mut dropped = Vec::new();
        for role in roles {
            if used.contains(&role.id.as_str()) {
                continue;
            }
            // 対話用の役割は、同名のハーネスが無ければ組み込みの `conversation` を上書きする。
            let target = if registry.contains(&role.id) {
                Some(role.id.clone())
            } else if role.id == conversation_genre {
                Some(BUILTIN_CONVERSATION.to_string())
            } else {
                None
            };
            match target {
                Some(id) => {
                    if let Some(spec) = registry.harnesses.iter_mut().find(|h| h.id == id) {
                        spec.apply_role(role);
                    }
                }
                None => dropped.push(role.id.clone()),
            }
        }
        (registry, dropped)
    }
}

/// ADR-0046 D1 の profile の検証に使う「知っているハーネス id」。設定の `[[genres]]`（= ハーネスの
/// 射影）に組み込みを足したもの。`genres` が空なら空を返す（最小構成では harness を検査しない）。
pub fn known_harness_ids(genres: &[GenreSpec]) -> Vec<String> {
    if genres.is_empty() {
        return Vec::new();
    }
    let mut ids: Vec<String> = genres.iter().map(|g| g.id.clone()).collect();
    for builtin in BUILTIN_HARNESSES {
        if !ids.iter().any(|id| id == builtin) {
            ids.push(builtin.to_string());
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy() -> (Vec<GenreSpec>, Vec<RoleSpec>) {
        let roles = vec![
            RoleSpec {
                id: "implementer".into(),
                tier: Some(Tier::Standard),
                adapter: Some("claude-code".into()),
                max_turns: Some(20),
                instructions: Some("あなたは実装担当。".into()),
                ..RoleSpec::default()
            },
            RoleSpec {
                id: "lead".into(),
                tier: Some(Tier::Frontier),
                ..RoleSpec::default()
            },
            RoleSpec {
                id: "secretary".into(),
                tier: Some(Tier::Standard),
                instructions: Some("あなたは人の秘書。".into()),
                ..RoleSpec::default()
            },
            RoleSpec {
                id: "reviewer".into(),
                tier: Some(Tier::Cheap),
                instructions: Some("条件を判定する。".into()),
                ..RoleSpec::default()
            },
        ];
        let genres = vec![
            GenreSpec {
                id: "secretary".into(),
                description: "人と話す".into(),
                default_role: Some("secretary".into()),
                roles: vec!["secretary".into()],
                ..GenreSpec::default()
            },
            GenreSpec {
                id: "coding".into(),
                description: "コードを書く".into(),
                capabilities: vec!["ソースコードの読み書き".into()],
                input_artifacts: vec!["repository".into()],
                output_artifacts: vec!["diff".into()],
                default_role: Some("implementer".into()),
                roles: vec!["lead".into(), "implementer".into()],
            },
        ];
        (genres, roles)
    }

    /// ADR-0046 §4-2: 旧い設定を読んで同じハーネス集合になる。
    #[test]
    fn from_legacy_maps_genres_to_harnesses_and_folds_the_default_role_in() {
        let (genres, roles) = legacy();
        let (registry, dropped) = HarnessRegistry::from_legacy(&genres, &roles, "secretary");
        let coding = registry.get("coding").expect("coding");
        assert_eq!(coding.adapter.as_deref(), Some("claude-code"));
        assert_eq!(coding.tier, Some(Tier::Standard));
        assert_eq!(coding.budget.max_turns, Some(20));
        assert_eq!(coding.instructions.as_deref(), Some("あなたは実装担当。"));
        assert_eq!(coding.capabilities, vec!["ソースコードの読み書き"]);
        assert!(!coding.conversation);
        // 対話用分野は `conversation = true`。
        let secretary = registry.get("secretary").expect("secretary");
        assert!(secretary.conversation);
        assert_eq!(
            secretary.instructions.as_deref(),
            Some("あなたは人の秘書。")
        );
        // 分野を持たない `reviewer` 役割は組み込みの `reviewer` ハーネスを上書きする。
        let reviewer = registry.get(BUILTIN_REVIEWER).expect("reviewer");
        assert_eq!(reviewer.tier, Some(Tier::Cheap));
        assert_eq!(reviewer.instructions.as_deref(), Some("条件を判定する。"));
        // 組み込みは全部ある。
        for id in BUILTIN_HARNESSES {
            assert!(registry.contains(id), "{id}");
        }
        // 写さなかったのは `lead` だけ（どの分野の default_role でもなく、同名のハーネスも無い）。
        assert_eq!(dropped, vec!["lead".to_string()]);
    }

    /// `[[harnesses]]` は組み込みを上書きできる。
    #[test]
    fn declared_harnesses_override_the_builtins() {
        let registry = HarnessRegistry::new(vec![HarnessSpec {
            id: BUILTIN_REVIEWER.into(),
            description: "わたしのレビュアー".into(),
            tier: Some(Tier::Frontier),
            ..HarnessSpec::default()
        }]);
        let reviewer = registry.get(BUILTIN_REVIEWER).expect("reviewer");
        assert_eq!(reviewer.description, "わたしのレビュアー");
        assert_eq!(reviewer.tier, Some(Tier::Frontier));
        assert_eq!(
            registry
                .all()
                .iter()
                .filter(|h| h.id == BUILTIN_REVIEWER)
                .count(),
            1
        );
        assert_eq!(registry.ids().len(), BUILTIN_HARNESSES.len());
        assert_eq!(registry.conversation_id(), Some(BUILTIN_CONVERSATION));
    }

    /// 互換の射影（`[[harnesses]]` → 旧い形）で、既存の経路がそのまま動く。
    #[test]
    fn the_registry_projects_back_to_genres_and_roles() {
        let registry = HarnessRegistry::new(vec![HarnessSpec {
            id: "coding".into(),
            description: "コードを書く".into(),
            adapter: Some("claude-code".into()),
            tier: Some(Tier::Standard),
            budget: HarnessBudget {
                max_turns: Some(60),
                max_wall_secs: Some(3600),
                max_retries: Some(1),
            },
            instructions: Some("実装担当".into()),
            output_artifacts: vec!["diff".into()],
            ..HarnessSpec::default()
        }]);
        let genres = registry.genre_specs();
        let g = GenreSpec::find(&genres, "coding").expect("coding");
        assert_eq!(g.default_role.as_deref(), Some("coding"));
        assert_eq!(g.roles, vec!["coding".to_string()]);
        assert_eq!(g.output_artifacts, vec!["diff".to_string()]);
        let roles = registry.role_specs();
        let r = RoleSpec::find(&roles, "coding").expect("coding");
        assert_eq!(r.adapter.as_deref(), Some("claude-code"));
        assert_eq!(r.max_turns, Some(60));
        assert_eq!(r.max_wall_secs, Some(3600));
        assert_eq!(r.instructions.as_deref(), Some("実装担当"));
        // ハーネス系の判定（ADR-0028 追記）もそのまま効く。
        assert_eq!(g.harness_adapter(&roles), Some("claude-code"));
    }

    #[test]
    fn known_harness_ids_adds_the_builtins_but_stays_empty_for_a_minimal_config() {
        assert!(known_harness_ids(&[]).is_empty());
        let genres = vec![GenreSpec {
            id: "coding".into(),
            ..GenreSpec::default()
        }];
        let ids = known_harness_ids(&genres);
        assert_eq!(ids[0], "coding");
        for builtin in BUILTIN_HARNESSES {
            assert!(ids.iter().any(|id| id == builtin), "{builtin}");
        }
    }

    /// ADR-0052 D2: `knowledge` ハーネスの組み込みの `fallback` は tier `cheap`。
    /// `[[harnesses]]` で同じ id を書いても（`fallback` を書かなければ）その既定を継ぐ。
    /// `fallback = false` と明示したときだけ無効になる。
    #[test]
    fn the_knowledge_harness_falls_back_to_cheap_by_default() {
        let registry = HarnessRegistry::builtins_only();
        let knowledge = registry.get(BUILTIN_KNOWLEDGE).expect("knowledge");
        assert_eq!(knowledge.fallback_tier(), Some(Tier::Cheap));
        // 他の組み込みは倒さない。
        assert_eq!(
            registry
                .get(BUILTIN_REVIEWER)
                .expect("reviewer")
                .fallback_tier(),
            None
        );

        // `fallback` を書かない上書きは組み込みの既定を継ぐ。
        let overridden = HarnessRegistry::new(vec![HarnessSpec {
            id: BUILTIN_KNOWLEDGE.into(),
            adapter: Some("langmem".into()),
            ..HarnessSpec::default()
        }]);
        assert_eq!(
            overridden
                .get(BUILTIN_KNOWLEDGE)
                .expect("knowledge")
                .fallback_tier(),
            Some(Tier::Cheap)
        );

        // `fallback = false` は無効。
        let disabled = HarnessRegistry::new(vec![HarnessSpec {
            id: BUILTIN_KNOWLEDGE.into(),
            fallback: Some(HarnessFallback::Switch(false)),
            ..HarnessSpec::default()
        }]);
        assert_eq!(
            disabled
                .get(BUILTIN_KNOWLEDGE)
                .expect("knowledge")
                .fallback_tier(),
            None
        );
    }

    /// ADR-0052 D2: `fallback = { tier = "frontier" }` と `fallback = false` の両方が TOML から読める。
    #[test]
    fn fallback_parses_from_both_toml_spellings() {
        #[derive(serde::Deserialize)]
        struct Wrapper {
            harnesses: Vec<HarnessSpec>,
        }
        let parsed: Wrapper = toml::from_str(
            r#"
[[harnesses]]
id = "a"
fallback = { tier = "frontier" }

[[harnesses]]
id = "b"
fallback = false

[[harnesses]]
id = "c"
"#,
        )
        .expect("parse");
        assert_eq!(parsed.harnesses[0].fallback_tier(), Some(Tier::Frontier));
        assert_eq!(parsed.harnesses[1].fallback_tier(), None);
        assert_eq!(parsed.harnesses[2].fallback, None);
        assert_eq!(
            HarnessFallback::Switch(true).tier(),
            Some(DEFAULT_FALLBACK_TIER)
        );
    }
}
