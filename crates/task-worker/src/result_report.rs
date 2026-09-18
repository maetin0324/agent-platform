//! 結果ファイルの `report`（ADR-0034 D7。Phase 27）。
//!
//! ワーカーは `done` を返すとき、その結果が「提案」なのか「ただの結果」なのかを**自分で宣言**できる:
//!
//! ```json
//! {"summary": "…", "evidence": [], "report": {"kind": "proposal"}}
//! ```
//!
//! ここは**ファイルを読んで文字列を取り出すだけ**で、`ReportKind` への写し替え（固定表）は
//! `task-dispatch` 側が行う。判断（この結果が提案に値するか）は taskd ではしない（DESIGN 原則 1）。
//! `memory`（ADR-0033 D6）と同じ流儀: 無い・JSON でない・形が違うときは `None`（run は失敗させない）。

use std::path::Path;

/// 結果ファイルの `report`（`kind` だけ。未知の値もそのまま文字列で持つ）。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
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
    let declaration: ReportDeclaration = serde_json::from_value(value.get("report")?.clone()).ok()?;
    let kind = declaration.kind?;
    if kind.trim().is_empty() { None } else { Some(kind) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_declared_kind_is_read_and_anything_else_is_none() {
        assert_eq!(
            report_kind_from_result_json(r#"{"summary":"s","report":{"kind":"proposal"}}"#).as_deref(),
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
        assert_eq!(report_kind_from_result_json(r#"{"report":{"kind":"  "}}"#), None);
        assert_eq!(report_kind_from_result_json(r#"{"report":"proposal"}"#), None);
        assert_eq!(report_kind_from_result_json("not json"), None);
    }

    #[test]
    fn a_missing_result_file_is_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        // ADR-0036 D2: 読むのは成果物ディレクトリの `result.json`（共有 workspace では `.taskd/artifacts/<id>/`）。
        let artifacts = dir.path().join(".taskd/artifacts/01HTASK");
        assert_eq!(read_result_report_kind(&artifacts), None);
        std::fs::create_dir_all(&artifacts).expect("mkdir");
        std::fs::write(artifacts.join("result.json"), r#"{"summary":"s","report":{"kind":"proposal"}}"#)
            .expect("write");
        assert_eq!(read_result_report_kind(&artifacts).as_deref(), Some("proposal"));
    }
}
