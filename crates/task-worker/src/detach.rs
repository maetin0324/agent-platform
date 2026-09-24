//! celeris（`celeris@<sha12>` unit）の cgroup の外で子プロセスを起こすための共通の小道具
//! （ADR-0060 D1、Phase 105 で共通化）。
//!
//! もとは `cluster_login.rs`（ssh master）だけが持っていた「`systemd-run --user --scope` に
//! 包むか、そのまま（`inline`）起こすか」の判断（[`resolve_detach_launcher`]）と
//! argv の組み立て（[`wrap_command`]）を、Phase 105 で `celeris::releases::start_promote`
//! （`promote.sh` の起こし方）も使うようになったのでここへ寄せた。
//!
//! - celeris の unit は `KillMode=control-group`（既定）で止まる。`systemd-run --user --scope`
//!   は指定したコマンドを **exec するだけ**（`Command::spawn` した子プロセスの pid がそのまま
//!   最終的に走るプログラムの pid になる）なので、celeris が直接起こした子はそのまま
//!   （cgroup だけが新しい scope に移る）。celeris の unit が止まっても、別 scope にいる子は
//!   巻き込まれない。
//! - どちらを使うかは `"auto"`（既定）/ `"systemd-run"` / `"inline"` の 3 値。`"auto"` は
//!   `systemd-run` が `PATH` にあり、かつ `XDG_RUNTIME_DIR` が設定されているときだけ
//!   `systemd-run` を選ぶ（本番の systemd user unit にはどちらも入っている。テストや非 systemd
//!   環境では自動的に `inline` に倒れる）。

/// 子プロセスの起こし方（ADR-0060 D1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetachLauncher {
    /// 従来どおり自分の直接の子として起こす（呼び出し元の cgroup の中に留まる）。
    Inline,
    /// `systemd-run --user --scope` で呼び出し元の cgroup の外の一時 scope に起こす。
    /// `program` は実行する `systemd-run`（本番は `PATH` 上の `"systemd-run"`。テストは偽物の絶対パス）。
    SystemdRun { program: String },
}

impl DetachLauncher {
    /// 本番で使う `SystemdRun`（`PATH` 上の `systemd-run` を使う）。
    pub fn systemd_run() -> Self {
        Self::SystemdRun {
            program: "systemd-run".to_string(),
        }
    }
}

/// 設定値（`"auto"` / `"systemd-run"` / `"inline"`）と環境から、実際に使う起こし方を決める。純関数。
///
/// - `"systemd-run"` / `"inline"`: そのまま使う（強制）。
/// - それ以外（`"auto"`。呼び出し側の `Config::validate` が他の値を弾いているので、想定外の値も
///   ここでは `auto` と同じに倒す）: `has_systemd_run && has_xdg_runtime_dir` のときだけ
///   `SystemdRun`、それ以外は `Inline`。
pub fn resolve_detach_launcher(
    configured: &str,
    has_systemd_run: bool,
    has_xdg_runtime_dir: bool,
) -> DetachLauncher {
    match configured {
        "systemd-run" => DetachLauncher::systemd_run(),
        "inline" => DetachLauncher::Inline,
        _ => {
            if has_systemd_run && has_xdg_runtime_dir {
                DetachLauncher::systemd_run()
            } else {
                DetachLauncher::Inline
            }
        }
    }
}

/// `path_env`（`PATH` の値）に実行可能な `name` があるか（`which name` 相当）。
pub fn path_has_executable(path_env: &str, name: &str) -> bool {
    std::env::split_paths(path_env).any(|dir| {
        let candidate = dir.join(name);
        std::fs::metadata(&candidate)
            .map(|m| m.is_file() && is_executable(&m))
            .unwrap_or(false)
    })
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

/// 実際のプロセス環境から `systemd-run` が `PATH` にあるか調べる（`resolve_detach_launcher` の
/// 呼び出し側が使う、非純粋な便利関数。判定そのものは [`resolve_detach_launcher`] が純関数で持つ）。
pub fn systemd_run_on_path() -> bool {
    std::env::var_os("PATH")
        .map(|p| path_has_executable(&p.to_string_lossy(), "systemd-run"))
        .unwrap_or(false)
}

/// 実際のプロセス環境から `XDG_RUNTIME_DIR` が（空でなく）設定されているか調べる。
pub fn xdg_runtime_dir_is_set() -> bool {
    std::env::var_os("XDG_RUNTIME_DIR").is_some_and(|v| !v.is_empty())
}

/// systemd のユニット名として安全な文字だけを残す（自由記述の id を埋め込むときの下ごしらえ）。
fn sanitize_unit_component(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `<prefix>-<safe id>-<短い乱数>` の scope unit 名。`id` は自由記述なので安全な文字だけに落とす。
pub fn scope_unit_name(prefix: &str, id: &str) -> String {
    let safe_id = sanitize_unit_component(id);
    let rand = task_core::TaskId::new().to_string();
    let short = &rand[rand.len().saturating_sub(8)..];
    format!("{prefix}-{safe_id}-{short}")
}

/// `launcher` に応じて、実際に spawn する `(program, args)` を組み立てる。純関数。
///
/// `Inline` はそのまま。`SystemdRun` は
/// `<program> --user --scope --quiet --unit <unit> --description <description> -- <program> <args...>`
/// を組み立てる。`--scope` は指定したコマンドを exec するだけなので、環境変数は `Command::envs` で
/// 渡したものがそのまま届く（exec は環境を消さない）。
pub fn wrap_command(
    launcher: &DetachLauncher,
    program: &str,
    args: &[String],
    unit: &str,
    description: &str,
) -> (String, Vec<String>) {
    match launcher {
        DetachLauncher::Inline => (program.to_string(), args.to_vec()),
        DetachLauncher::SystemdRun { program: runner } => {
            let mut full = vec![
                "--user".to_string(),
                "--scope".to_string(),
                "--quiet".to_string(),
                "--unit".to_string(),
                unit.to_string(),
                "--description".to_string(),
                description.to_string(),
                "--".to_string(),
                program.to_string(),
            ];
            full.extend(args.iter().cloned());
            (runner.clone(), full)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用: `path` に実行可能なファイルを書く（`cluster_login.rs`/`test_support.rs` と同じ流儀）。
    fn write_executable(path: &std::path::Path, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, script).expect("write script");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    #[test]
    fn resolve_detach_launcher_auto_needs_both_conditions() {
        assert!(matches!(
            resolve_detach_launcher("auto", true, true),
            DetachLauncher::SystemdRun { .. }
        ));
        assert!(matches!(
            resolve_detach_launcher("auto", false, true),
            DetachLauncher::Inline
        ));
        assert!(matches!(
            resolve_detach_launcher("auto", true, false),
            DetachLauncher::Inline
        ));
        assert!(matches!(
            resolve_detach_launcher("auto", false, false),
            DetachLauncher::Inline
        ));
    }

    #[test]
    fn resolve_detach_launcher_explicit_values_ignore_the_environment() {
        assert!(matches!(
            resolve_detach_launcher("systemd-run", false, false),
            DetachLauncher::SystemdRun { .. }
        ));
        assert!(matches!(
            resolve_detach_launcher("inline", true, true),
            DetachLauncher::Inline
        ));
    }

    #[test]
    fn wrap_command_inline_is_unchanged() {
        let args = vec!["-M".to_string(), "-N".to_string(), "pegasus".to_string()];
        let (program, full_args) = wrap_command(&DetachLauncher::Inline, "ssh", &args, "u", "d");
        assert_eq!(program, "ssh");
        assert_eq!(full_args, args);
    }

    #[test]
    fn wrap_command_systemd_run_builds_the_scope_invocation() {
        let launcher = DetachLauncher::SystemdRun {
            program: "systemd-run".to_string(),
        };
        let args = vec!["-M".to_string(), "-N".to_string(), "pegasus".to_string()];
        let (program, full_args) = wrap_command(
            &launcher,
            "ssh",
            &args,
            "celeris-ssh-master-pegasus-abcd1234",
            "celeris ssh master (pegasus)",
        );
        assert_eq!(program, "systemd-run");
        assert_eq!(
            full_args,
            vec![
                "--user".to_string(),
                "--scope".to_string(),
                "--quiet".to_string(),
                "--unit".to_string(),
                "celeris-ssh-master-pegasus-abcd1234".to_string(),
                "--description".to_string(),
                "celeris ssh master (pegasus)".to_string(),
                "--".to_string(),
                "ssh".to_string(),
                "-M".to_string(),
                "-N".to_string(),
                "pegasus".to_string(),
            ]
        );
    }

    #[test]
    fn scope_unit_name_sanitizes_the_id() {
        let unit = scope_unit_name("celeris-promote", "weird id/../x");
        assert!(unit.starts_with("celeris-promote-weird_id____x-"), "{unit}");
    }

    #[test]
    fn path_has_executable_finds_an_executable_file_in_one_of_the_path_dirs() {
        let empty_dir = tempfile::tempdir().unwrap();
        let bin_dir = tempfile::tempdir().unwrap();
        let path_env = std::env::join_paths([empty_dir.path(), bin_dir.path()])
            .unwrap()
            .into_string()
            .unwrap();
        assert!(!path_has_executable(&path_env, "systemd-run"));

        write_executable(&bin_dir.path().join("systemd-run"), "#!/bin/sh\nexit 0\n");
        assert!(path_has_executable(&path_env, "systemd-run"));
    }

    #[test]
    fn path_has_executable_ignores_non_executable_files() {
        let bin_dir = tempfile::tempdir().unwrap();
        std::fs::write(bin_dir.path().join("systemd-run"), "not executable").unwrap();
        let path_env = bin_dir.path().to_string_lossy().into_owned();
        assert!(!path_has_executable(&path_env, "systemd-run"));
    }
}
