//! ADR-0056 D3（Phase 79）: mount された skills（`RunContext.skills`）を run 開始時にアダプタへ届ける。
//!
//! `RunContext.skills` は名前・KB のディレクトリの絶対パス・frontmatter の説明だけを運ぶ（本文は
//! 含まない）。ここは本文（`SKILL.md`）を実際に読み、アダプタごとの届け先に書く/組み立てる
//! （`claude_code` / `codex` / `acp` から呼ばれる。研究系アダプタは呼ばない）。
//!
//! 届け方（ADR-0056 D3）:
//! - `claude-code`: 作業場所の `.claude/skills/<name>/` に KB のディレクトリを丸ごと写す
//!   （そのサブディレクトリだけを置き換える。`.claude` の他の内容には触れない）。
//! - `codex`: 作業場所の `AGENTS.md` の末尾の `<!-- celeris:skills:start -->` 〜
//!   `<!-- celeris:skills:end -->` の間を run ごとに書き直す（既存の `AGENTS.md` の他の内容は保つ。
//!   無ければ作る）。
//! - `acp`: 前置き（プロンプト文面）に `## Skills（celeris）` 節として直接埋め込む。
//!
//! ADR-0056 Phase 81 追記: `claude-code` は unmount した skill のディレクトリが `.claude/skills/`
//! に残り続ける問題があった（Phase 79 は「対象の skill だけ置き換える」までしかやらず、
//! 「前回あったが今回は無い」skill の削除は範囲外だった）。`<cwd>/.celeris/skills.json` に
//! 前回 celeris が書いた skill 名の一覧を残し、次回の届け先でそこにあって今回無い名前だけを
//! 消す（マーカーに無いディレクトリ＝人・他の仕組みが置いたものには触れない）。`codex` の
//! `AGENTS.md` は区切りの節を run ごとに丸ごと書き直す（`rewrite_agents_md`）ので、同じ問題は
//! 元から無い（マーカーは要らない）。

use std::path::Path;
use std::pin::Pin;

use crate::protocol::SkillMount;

/// Phase 81: `claude-code` が前回書いた skill 名の一覧（`.celeris/skills.json`）。
const SKILLS_MARKER_REL: &str = ".celeris/skills.json";

fn skills_marker_path(cwd: &Path) -> std::path::PathBuf {
    cwd.join(SKILLS_MARKER_REL)
}

/// マーカーを読む。無い・壊れている・読めないときは空（＝前回の記録が無いものとして扱う。
/// 削除を試みない方が安全 — ユーザーが手で作ったディレクトリを誤って消さないため）。
async fn read_skills_marker(cwd: &Path) -> Vec<String> {
    match tokio::fs::read_to_string(skills_marker_path(cwd)).await {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// マーカーを書く（`.celeris/` が無ければ作る）。書けなくても run は落とさない（呼び出し側が warn する）。
async fn write_skills_marker(cwd: &Path, names: &[String]) -> std::io::Result<()> {
    let path = skills_marker_path(cwd);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let json = serde_json::to_string(names).unwrap_or_else(|_| "[]".to_string());
    tokio::fs::write(&path, json).await
}

/// `codex` の `AGENTS.md` に足す節の区切り。この 2 行の間だけを run ごとに書き直す。
pub const SECTION_BEGIN: &str = "<!-- celeris:skills:start -->";
pub const SECTION_END: &str = "<!-- celeris:skills:end -->";

/// KB の `<mount.path>/SKILL.md` を読む（読めなければ空文字列。run は落とさない）。
fn read_skill_md(mount: &SkillMount) -> String {
    std::fs::read_to_string(Path::new(&mount.path).join("SKILL.md")).unwrap_or_default()
}

/// `## Skills（celeris）` 節の中身（区切り行は含まない）。`skills` が空なら `None`。
/// 各 skill を `### <name>` の見出し + description（あれば）+ `SKILL.md` の本文で連結する。
pub fn skills_block(skills: &[SkillMount]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let mut out = String::from("## Skills（celeris）\n");
    for mount in skills {
        out.push_str(&format!("\n### {}\n", mount.name));
        if !mount.description.is_empty() {
            out.push_str(&mount.description);
            out.push('\n');
        }
        let body = read_skill_md(mount);
        let body = body.trim_end();
        if !body.is_empty() {
            out.push('\n');
            out.push_str(body);
            out.push('\n');
        }
    }
    Some(out)
}

/// `AGENTS.md` の中身（`existing`）の末尾にある区切りの節を書き直す。`block`（`skills_block` の結果）が
/// `Some` ならその節を末尾に置く（無ければ足す）。`None` なら既存の節を取り除くだけ（新しい節は足さない）。
/// 区切りの**外側**の内容は常にそのまま保つ。冪等（同じ入力を 2 回かけても結果は変わらない）。
pub fn rewrite_agents_md(existing: &str, block: Option<&str>) -> String {
    let mut base = existing.to_string();
    if let (Some(start), Some(end_tag_at)) = (base.find(SECTION_BEGIN), base.find(SECTION_END)) {
        let end = end_tag_at + SECTION_END.len();
        if start <= end {
            base.replace_range(start..end, "");
        }
    }
    let base = base.trim_end();
    let Some(block) = block else {
        return if base.is_empty() {
            String::new()
        } else {
            format!("{base}\n")
        };
    };
    let mut out = base.to_string();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(SECTION_BEGIN);
    out.push('\n');
    out.push_str(block.trim_end());
    out.push('\n');
    out.push_str(SECTION_END);
    out.push('\n');
    out
}

/// `codex` 用: `<cwd>/AGENTS.md` を読み直し（無ければ空から）、skills の節を書き直す。`skills` が空なら
/// 何もしない（ファイルが無いのに作らない。既存の `AGENTS.md` にも触れない）。
pub async fn deliver_agents_md(cwd: &Path, skills: &[SkillMount]) -> std::io::Result<()> {
    if skills.is_empty() {
        return Ok(());
    }
    let path = cwd.join("AGENTS.md");
    let existing = tokio::fs::read_to_string(&path).await.unwrap_or_default();
    let updated = rewrite_agents_md(&existing, skills_block(skills).as_deref());
    tokio::fs::write(&path, updated).await
}

/// `acp` 用: 前置き（プロンプト文面）の末尾に足す文字列（`skills` が空なら空文字列）。
pub fn preamble_section(skills: &[SkillMount]) -> String {
    match skills_block(skills) {
        Some(block) => format!("\n{block}"),
        None => String::new(),
    }
}

type BoxFuture<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// `src` の中身（サブディレクトリ含む）を `dest` に丸ごと写す。
fn copy_dir<'a>(src: &'a Path, dest: &'a Path) -> BoxFuture<'a, std::io::Result<()>> {
    Box::pin(async move {
        tokio::fs::create_dir_all(dest).await?;
        let mut entries = tokio::fs::read_dir(src).await?;
        while let Some(entry) = entries.next_entry().await? {
            let file_type = entry.file_type().await?;
            let from = entry.path();
            let to = dest.join(entry.file_name());
            if file_type.is_dir() {
                copy_dir(&from, &to).await?;
            } else if file_type.is_file() {
                tokio::fs::copy(&from, &to).await?;
            }
            // シンボリックリンク等は写さない（KB のディレクトリは celeris 自身が書くので想定しない）。
        }
        Ok(())
    })
}

/// `claude-code` 用: KB の `<mount.path>/` を丸ごと `<cwd>/.claude/skills/<name>/` へ写す
/// （そのディレクトリ**だけ**を置き換える。`.claude` の他の内容には触れない）。
///
/// Phase 81: 前回このワーカーが書いた skill 名（`.celeris/skills.json`）を読み、今回の `skills` に
/// 無い名前だけ `.claude/skills/<name>/` を削除する（マーカーに無い名前＝人・他の仕組みが置いた
/// ディレクトリには触れない）。`skills` が空でも、前回の記録があれば掃除だけは行う（マーカーも
/// 空にする）。前回の記録が無く今回も空なら、何も無いので `.claude` すら作らない（従来どおり）。
pub async fn deliver_claude_code(cwd: &Path, skills: &[SkillMount]) -> std::io::Result<()> {
    let previous = read_skills_marker(cwd).await;
    let current_names: Vec<String> = skills.iter().map(|m| m.name.clone()).collect();

    if !previous.is_empty() {
        let root = cwd.join(".claude").join("skills");
        for name in &previous {
            if current_names.contains(name) {
                continue;
            }
            let dest = root.join(name);
            if tokio::fs::try_exists(&dest).await.unwrap_or(false) {
                tokio::fs::remove_dir_all(&dest).await?;
            }
        }
    }

    if skills.is_empty() {
        if !previous.is_empty() {
            write_skills_marker(cwd, &current_names).await?;
        }
        return Ok(());
    }

    let root = cwd.join(".claude").join("skills");
    tokio::fs::create_dir_all(&root).await?;
    for mount in skills {
        let dest = root.join(&mount.name);
        if tokio::fs::try_exists(&dest).await.unwrap_or(false) {
            tokio::fs::remove_dir_all(&dest).await?;
        }
        copy_dir(Path::new(&mount.path), &dest).await?;
    }
    write_skills_marker(cwd, &current_names).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(
        dir: &std::path::Path,
        name: &str,
        description: &str,
        body_extra: &str,
    ) -> SkillMount {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n\n本文{body_extra}\n"
            ),
        )
        .unwrap();
        SkillMount {
            name: name.to_string(),
            path: skill_dir.display().to_string(),
            description: description.to_string(),
        }
    }

    #[test]
    fn skills_block_is_none_for_empty_skills() {
        assert_eq!(skills_block(&[]), None);
    }

    #[test]
    fn skills_block_concatenates_name_description_and_body() {
        let dir = tempfile::tempdir().unwrap();
        let mount = write_skill(dir.path(), "rust-review", "Rust のレビュー観点", "");
        let block = skills_block(std::slice::from_ref(&mount)).unwrap();
        assert!(block.starts_with("## Skills（celeris）\n"));
        assert!(block.contains("### rust-review\n"));
        assert!(block.contains("Rust のレビュー観点"));
        assert!(block.contains("# rust-review"));
        assert!(block.contains("本文"));
    }

    #[test]
    fn skills_block_survives_a_missing_skill_md_without_panicking() {
        let mount = SkillMount {
            name: "ghost".to_string(),
            path: "/does/not/exist".to_string(),
            description: String::new(),
        };
        let block = skills_block(&[mount]).unwrap();
        assert!(block.contains("### ghost"));
    }

    #[test]
    fn rewrite_agents_md_appends_the_section_to_an_empty_file() {
        let out = rewrite_agents_md("", Some("## Skills（celeris）\n\n### a\nbody\n"));
        assert!(out.starts_with(SECTION_BEGIN));
        assert!(out.trim_end().ends_with(SECTION_END));
        assert!(out.contains("### a"));
    }

    /// 既存の `AGENTS.md` の内容は壊さず、節はその後ろに足される。
    #[test]
    fn rewrite_agents_md_preserves_existing_content() {
        let existing = "# Project notes\n\nDo not break the build.\n";
        let out = rewrite_agents_md(existing, Some("## Skills（celeris）\n\n### a\nbody\n"));
        assert!(out.starts_with(existing.trim_end()));
        assert!(out.contains(SECTION_BEGIN));
        assert!(out.contains("### a"));
    }

    /// 2 回かけても中身が増殖しない（冪等）。2 回目は 1 回目の出力を `existing` として渡す。
    #[test]
    fn rewrite_agents_md_is_idempotent() {
        let existing = "# Project notes\n";
        let once = rewrite_agents_md(existing, Some("## Skills（celeris）\n\n### a\nbody\n"));
        let twice = rewrite_agents_md(&once, Some("## Skills（celeris）\n\n### a\nbody\n"));
        assert_eq!(once, twice);
        assert_eq!(twice.matches(SECTION_BEGIN).count(), 1);
        assert_eq!(twice.matches("### a").count(), 1);
    }

    /// 節の中身が変われば（skill が増減すれば）次の run で置き換わる。
    #[test]
    fn rewrite_agents_md_replaces_the_section_when_the_block_changes() {
        let existing = "# Project notes\n";
        let once = rewrite_agents_md(existing, Some("## Skills（celeris）\n\n### a\nbody\n"));
        let twice = rewrite_agents_md(&once, Some("## Skills（celeris）\n\n### b\nother\n"));
        assert!(!twice.contains("### a"));
        assert!(twice.contains("### b"));
        assert_eq!(twice.matches(SECTION_BEGIN).count(), 1);
    }

    /// `block = None`（skills が無くなった）なら節を取り除くだけ。
    #[test]
    fn rewrite_agents_md_removes_the_section_when_the_block_is_none() {
        let existing = "# Project notes\n";
        let once = rewrite_agents_md(existing, Some("## Skills（celeris）\n\n### a\nbody\n"));
        let removed = rewrite_agents_md(&once, None);
        assert!(!removed.contains(SECTION_BEGIN));
        assert!(removed.contains("# Project notes"));
    }

    #[test]
    fn preamble_section_is_empty_for_no_skills() {
        assert_eq!(preamble_section(&[]), "");
    }

    #[test]
    fn preamble_section_carries_the_skills_block() {
        let dir = tempfile::tempdir().unwrap();
        let mount = write_skill(dir.path(), "writing", "文章の書き方", "");
        let section = preamble_section(std::slice::from_ref(&mount));
        assert!(section.contains("## Skills（celeris）"));
        assert!(section.contains("### writing"));
    }

    #[tokio::test]
    async fn deliver_claude_code_writes_skill_md_and_sibling_files() {
        let kb = tempfile::tempdir().unwrap();
        let skill_dir = kb.path().join("rust-review");
        std::fs::create_dir_all(skill_dir.join("references")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: rust-review\ndescription: d\n---\n\nbody\n",
        )
        .unwrap();
        std::fs::write(skill_dir.join("references/checklist.md"), "1. …\n").unwrap();
        let mount = SkillMount {
            name: "rust-review".to_string(),
            path: skill_dir.display().to_string(),
            description: "d".to_string(),
        };

        let cwd = tempfile::tempdir().unwrap();
        deliver_claude_code(cwd.path(), &[mount]).await.unwrap();

        let dest = cwd.path().join(".claude/skills/rust-review");
        assert!(dest.join("SKILL.md").exists());
        assert_eq!(
            std::fs::read_to_string(dest.join("references/checklist.md")).unwrap(),
            "1. …\n"
        );
    }

    /// 既存の `.claude/` の他の内容（別の skill・別の設定ファイル）には触れない。対象の skill の
    /// ディレクトリだけを置き換える。
    #[tokio::test]
    async fn deliver_claude_code_only_touches_its_own_skill_directories() {
        let kb = tempfile::tempdir().unwrap();
        let skill_dir = kb.path().join("writing");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: writing\ndescription: d\n---\n\nnew body\n",
        )
        .unwrap();
        let mount = SkillMount {
            name: "writing".to_string(),
            path: skill_dir.display().to_string(),
            description: "d".to_string(),
        };

        let cwd = tempfile::tempdir().unwrap();
        // 既存の `.claude/skills/other-skill/` と `.claude/settings.json` は人・別の仕組みが置いたもの。
        std::fs::create_dir_all(cwd.path().join(".claude/skills/other-skill")).unwrap();
        std::fs::write(
            cwd.path().join(".claude/skills/other-skill/SKILL.md"),
            "untouched\n",
        )
        .unwrap();
        std::fs::write(cwd.path().join(".claude/settings.json"), "{}").unwrap();
        // 古い `writing` の中身（今回の mount で置き換わるはず）。
        std::fs::create_dir_all(cwd.path().join(".claude/skills/writing")).unwrap();
        std::fs::write(
            cwd.path().join(".claude/skills/writing/SKILL.md"),
            "stale body\n",
        )
        .unwrap();
        std::fs::write(
            cwd.path().join(".claude/skills/writing/stale.txt"),
            "gone\n",
        )
        .unwrap();

        deliver_claude_code(cwd.path(), &[mount]).await.unwrap();

        assert_eq!(
            std::fs::read_to_string(cwd.path().join(".claude/skills/writing/SKILL.md")).unwrap(),
            "---\nname: writing\ndescription: d\n---\n\nnew body\n"
        );
        assert!(!cwd.path().join(".claude/skills/writing/stale.txt").exists());
        assert_eq!(
            std::fs::read_to_string(cwd.path().join(".claude/skills/other-skill/SKILL.md"))
                .unwrap(),
            "untouched\n"
        );
        assert_eq!(
            std::fs::read_to_string(cwd.path().join(".claude/settings.json")).unwrap(),
            "{}"
        );
    }

    #[tokio::test]
    async fn deliver_claude_code_does_nothing_for_empty_skills() {
        let cwd = tempfile::tempdir().unwrap();
        deliver_claude_code(cwd.path(), &[]).await.unwrap();
        assert!(!cwd.path().join(".claude").exists());
    }

    #[tokio::test]
    async fn deliver_agents_md_does_nothing_for_empty_skills() {
        let cwd = tempfile::tempdir().unwrap();
        deliver_agents_md(cwd.path(), &[]).await.unwrap();
        assert!(!cwd.path().join("AGENTS.md").exists());
    }

    #[tokio::test]
    async fn deliver_agents_md_creates_the_file_and_preserves_reruns() {
        let dir = tempfile::tempdir().unwrap();
        let mount = write_skill(dir.path(), "rust-review", "d", "");
        let cwd = tempfile::tempdir().unwrap();
        std::fs::write(
            cwd.path().join("AGENTS.md"),
            "# Notes\n\nBuild with cargo.\n",
        )
        .unwrap();

        deliver_agents_md(cwd.path(), std::slice::from_ref(&mount))
            .await
            .unwrap();
        let first = std::fs::read_to_string(cwd.path().join("AGENTS.md")).unwrap();
        assert!(first.contains("# Notes"));
        assert!(first.contains("Build with cargo."));
        assert!(first.contains(SECTION_BEGIN));
        assert!(first.contains("### rust-review"));

        // 同じ skills でもう一度届けても増殖しない。
        deliver_agents_md(cwd.path(), std::slice::from_ref(&mount))
            .await
            .unwrap();
        let second = std::fs::read_to_string(cwd.path().join("AGENTS.md")).unwrap();
        assert_eq!(first, second);
    }

    // ---- ADR-0056 Phase 81 追記: unmount 後の stale な skill ディレクトリの掃除 ----

    /// mount A+B → マーカーに両方が載る。次に mount A だけにすると、B のディレクトリは消え、
    /// マーカーに無い（celeris が書いていない）C は生き残る。
    #[tokio::test]
    async fn deliver_claude_code_removes_unmounted_directories_but_keeps_user_authored_ones() {
        let kb = tempfile::tempdir().unwrap();
        let mount_a = write_skill(kb.path(), "a", "skill a", "");
        let mount_b = write_skill(kb.path(), "b", "skill b", "");
        let cwd = tempfile::tempdir().unwrap();

        // ユーザーが自分で置いた C（celeris は一度も書いていない）。
        std::fs::create_dir_all(cwd.path().join(".claude/skills/c")).unwrap();
        std::fs::write(
            cwd.path().join(".claude/skills/c/SKILL.md"),
            "user authored\n",
        )
        .unwrap();

        // 1 回目: A + B を mount。
        deliver_claude_code(cwd.path(), &[mount_a.clone(), mount_b.clone()])
            .await
            .unwrap();
        assert!(cwd.path().join(".claude/skills/a/SKILL.md").exists());
        assert!(cwd.path().join(".claude/skills/b/SKILL.md").exists());
        assert!(cwd.path().join(".claude/skills/c/SKILL.md").exists());
        let marker_path = cwd.path().join(".celeris/skills.json");
        let marker: Vec<String> =
            serde_json::from_str(&std::fs::read_to_string(&marker_path).unwrap()).unwrap();
        assert_eq!(marker, vec!["a".to_string(), "b".to_string()]);

        // 2 回目: A だけを mount。B は消え、C（マーカーに無い）は残る。
        deliver_claude_code(cwd.path(), &[mount_a]).await.unwrap();
        assert!(cwd.path().join(".claude/skills/a/SKILL.md").exists());
        assert!(
            !cwd.path().join(".claude/skills/b").exists(),
            "b was unmounted; its directory must be removed"
        );
        assert_eq!(
            std::fs::read_to_string(cwd.path().join(".claude/skills/c/SKILL.md")).unwrap(),
            "user authored\n",
            "c is not in the marker (celeris never wrote it); it must survive"
        );
        let marker: Vec<String> =
            serde_json::from_str(&std::fs::read_to_string(&marker_path).unwrap()).unwrap();
        assert_eq!(marker, vec!["a".to_string()]);
    }

    /// mount A+B の後に skills が空になったら、A と B の両方が消え、マーカーも空になる
    /// （マーカーに無い C は触れない）。
    #[tokio::test]
    async fn deliver_claude_code_removes_all_previously_mounted_when_skills_becomes_empty() {
        let kb = tempfile::tempdir().unwrap();
        let mount_a = write_skill(kb.path(), "a", "skill a", "");
        let mount_b = write_skill(kb.path(), "b", "skill b", "");
        let cwd = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(cwd.path().join(".claude/skills/c")).unwrap();
        std::fs::write(cwd.path().join(".claude/skills/c/SKILL.md"), "user\n").unwrap();

        deliver_claude_code(cwd.path(), &[mount_a, mount_b])
            .await
            .unwrap();
        deliver_claude_code(cwd.path(), &[]).await.unwrap();

        assert!(!cwd.path().join(".claude/skills/a").exists());
        assert!(!cwd.path().join(".claude/skills/b").exists());
        assert!(cwd.path().join(".claude/skills/c").exists());
        let marker_path = cwd.path().join(".celeris/skills.json");
        let marker: Vec<String> =
            serde_json::from_str(&std::fs::read_to_string(&marker_path).unwrap()).unwrap();
        assert!(marker.is_empty());
    }

    /// `codex`: mount A+B → AGENTS.md の節に両方。次に mount A だけにすると、節から B が消え、
    /// A だけ残る（`rewrite_agents_md` が節を丸ごと書き直すので、`claude-code` のような別マーカーは
    /// 要らない）。
    #[tokio::test]
    async fn deliver_agents_md_shrinks_the_section_when_a_skill_is_unmounted() {
        let dir = tempfile::tempdir().unwrap();
        let mount_a = write_skill(dir.path(), "a", "skill a", "");
        let mount_b = write_skill(dir.path(), "b", "skill b", "");
        let cwd = tempfile::tempdir().unwrap();

        deliver_agents_md(cwd.path(), &[mount_a.clone(), mount_b])
            .await
            .unwrap();
        let with_both = std::fs::read_to_string(cwd.path().join("AGENTS.md")).unwrap();
        assert!(with_both.contains("### a"));
        assert!(with_both.contains("### b"));

        deliver_agents_md(cwd.path(), &[mount_a]).await.unwrap();
        let with_one = std::fs::read_to_string(cwd.path().join("AGENTS.md")).unwrap();
        assert!(with_one.contains("### a"));
        assert!(
            !with_one.contains("### b"),
            "b was unmounted; the section must shrink to just a"
        );
    }
}
