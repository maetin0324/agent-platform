//! `celerisctl routing show`（ADR-0069 Phase 118 D3）。
//!
//! 設定ファイルだけを読む読み取り専用コマンド（`config to-harnesses` と同じく DB を開かない）。
//! 2 つの表を出す: (1) `[[providers]] tier_models` の provider ごとの
//! `tier → name / model_id（または unavailable の理由）/ reasoning_effort`、
//! (2) `[llm_proxy.models]` の `tier → claude/gpt/qwen ごとの model`。
//! どちらも設定ファイルに書かれた値をそのまま見せるだけで、到達性・残量は見ない
//! （到達性・cooldown は既存の `GET /llm/sources` の仕事）。

use std::path::PathBuf;
use std::process::ExitCode;

use celeris::config::Config;
use clap::Subcommand;
use task_core::Tier;

use crate::error::CliError;
use crate::outln;

/// 表示順（頻度の高い順ではなく、上から強い lane の順。GUI の `TIER_OPTIONS` と同じ並び）。
const TIER_ORDER: [Tier; 3] = [Tier::Frontier, Tier::Standard, Tier::Cheap];

#[derive(Subcommand, Debug)]
pub enum RoutingCommand {
    /// provider ごとの tier → 実行モデル/effort と、`[llm_proxy.models]` の tier → 供給元ごとの
    /// モデルを表にして出す。
    Show(RoutingShowArgs),
}

#[derive(clap::Args, Debug)]
pub struct RoutingShowArgs {
    /// 読み込む設定ファイル（`~/.config/celeris/config.toml` など）。
    #[arg(long)]
    pub config: PathBuf,
}

pub fn run(command: RoutingCommand) -> Result<ExitCode, CliError> {
    match command {
        RoutingCommand::Show(args) => {
            let config = Config::load(&args.config)
                .map_err(|e| CliError::msg(format!("config {}: {e}", args.config.display())))?;
            outln!("{}", render_routing_table(&config).trim_end());
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn tier_label(tier: Tier) -> &'static str {
    match tier {
        Tier::Frontier => "frontier",
        Tier::Standard => "standard",
        Tier::Cheap => "cheap",
    }
}

/// provider の 1 tier の表示（`ModelBinding` が無ければ「未設定」、`unavailable_reason` があれば
/// それを優先して見せる。`model_routing::resolve` と同じ優先順）。
fn binding_cell(binding: Option<&task_core::model_routing::ModelBinding>) -> String {
    match binding {
        None => "(未設定)".to_string(),
        Some(b) => {
            let mut cell = b.name.clone();
            if let Some(reason) = &b.unavailable_reason {
                cell.push_str(&format!(" -> unavailable: {reason}"));
            } else if let Some(id) = &b.model_id {
                cell.push_str(&format!(" -> {id}"));
            } else {
                cell.push_str(" -> (model_id 未設定)");
            }
            if let Some(effort) = &b.reasoning_effort {
                cell.push_str(&format!(" [effort={effort}]"));
            }
            cell
        }
    }
}

/// `celerisctl routing show` の本体（純粋関数。テストしやすいよう `run` から分離）。
pub fn render_routing_table(config: &Config) -> String {
    let mut out = String::new();
    out.push_str("=== providers (tier_models) ===\n");
    if config.providers.is_empty() {
        out.push_str("(no providers configured)\n");
    }
    for p in &config.providers {
        if p.tier_models.is_empty() {
            out.push_str(&format!(
                "{} ({}): tier_models not configured (legacy single model = {:?})\n",
                p.id,
                p.adapter,
                if p.model.is_empty() {
                    "adapter default".to_string()
                } else {
                    p.model.clone()
                }
            ));
            continue;
        }
        out.push_str(&format!("{} ({}):\n", p.id, p.adapter));
        for tier in TIER_ORDER {
            out.push_str(&format!(
                "  {:<8} {}\n",
                tier_label(tier),
                binding_cell(p.tier_models.get(&tier))
            ));
        }
    }
    out.push('\n');
    out.push_str("=== [llm_proxy.models] (tier -> source model) ===\n");
    out.push_str(&format!(
        "  {:<8} {:<24} {:<24} {}\n",
        "tier", "claude", "gpt", "qwen"
    ));
    for tier in TIER_ORDER {
        let claude = config
            .llm_proxy
            .models
            .claude
            .get(&tier)
            .map(String::as_str)
            .unwrap_or("(未設定)");
        let gpt = config
            .llm_proxy
            .models
            .gpt
            .get(&tier)
            .map(String::as_str)
            .unwrap_or("(未設定)");
        let qwen = config
            .llm_proxy
            .models
            .qwen
            .get(&tier)
            .map(String::as_str)
            .unwrap_or("(未設定)");
        out.push_str(&format!(
            "  {:<8} {:<24} {:<24} {}\n",
            tier_label(tier),
            claude,
            gpt,
            qwen
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(dir: &std::path::Path, text: &str) -> PathBuf {
        let path = dir.join("config.toml");
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn shows_provider_tier_models_and_llm_proxy_models_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            dir.path(),
            r#"
[[providers]]
id = "claude"
adapter = "claude-code"
[providers.tier_models.frontier]
name = "fable"
model_id = "claude-fable-5-1"
[providers.tier_models.standard]
name = "opus"
unavailable_reason = "not verified yet"
[providers.tier_models.cheap]
name = "sonnet"
model_id = "claude-sonnet-5"

[[providers]]
id = "legacy"
adapter = "fake"
"#,
        );
        let config = Config::load(&path).unwrap();
        let table = render_routing_table(&config);
        assert!(table.contains("claude (claude-code):"));
        assert!(table.contains("frontier fable -> claude-fable-5-1"));
        assert!(table.contains("standard opus -> unavailable: not verified yet"));
        assert!(table.contains("cheap    sonnet -> claude-sonnet-5"));
        assert!(table.contains("legacy (fake): tier_models not configured"));
        // llm_proxy.models の既定表（ADR-0069 Phase 118 D2）。
        assert!(table.contains("[llm_proxy.models]"));
        assert!(table.contains("claude-fable-5-1"));
        assert!(table.contains("gpt-6-astra"));
        assert!(table.contains("qwen3.8-27b"));
    }

    #[test]
    fn a_provider_without_a_tier_binding_is_marked_unset() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            dir.path(),
            r#"
[[providers]]
id = "gpt"
adapter = "codex"
[providers.tier_models.frontier]
name = "astra"
model_id = "gpt-6-astra"
reasoning_effort = "high"
"#,
        );
        let config = Config::load(&path).unwrap();
        let table = render_routing_table(&config);
        assert!(table.contains("frontier astra -> gpt-6-astra [effort=high]"));
        assert!(table.contains("standard (未設定)"));
        assert!(table.contains("cheap    (未設定)"));
    }
}
