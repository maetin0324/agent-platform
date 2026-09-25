//! `celeris` バイナリ。`celeris --config <path> [--until-idle] [--max-ticks N]`（ADR-0005 D7）。
//! ADR-0040 D3 / D4（Phase 47）: `--mode` / `--db` / `--listen` / `--workspace-root` / `--token-file` /
//! `--release` の上書き。終了コードは 0（正常・drain 完了）/ 2（設定・スキーマ）/ 3（同じ版の二重起動）。
//!
//! Phase 119 D1: `#[tokio::main]` は使わない。あのマクロが組む `Runtime` は、`main` の中身が
//! 返った**後**に暗黙に drop され、その drop は `spawn_blocking` のブロッキングスレッドプールが
//! 空になるまで**無期限に**待つ。本番 2026-09-24 で、`drain` を終えて「celeris stopped, exit: Drained」
//! まで journal に出た後もプロセスが終了せず、`SIGTERM` すら効かない（＝独自の SIGTERM ハンドラを
//! 登録した後に、それを汲み取るイベントループ自体が動いていないと、既定の「終了する」という挙動が
//! 失われる）事故が起きた。ここでは runtime を手で組み、`run()` が返った後は
//! `Runtime::shutdown_timeout`（上限つき）で必ず先へ進み、その後 `std::process::exit` で
//! **確実に**プロセスを終える（生き残ったブロッキングスレッドは OS がプロセスごと片付ける）。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use celeris::{Config, Overrides, RunOptions};
use clap::Parser;

/// ADR-0045 D2: `--config` の既定値（`~` は `$HOME` で展開する）。
const DEFAULT_CONFIG: &str = "~/.config/celeris/config.toml";

#[derive(Parser, Debug)]
#[command(
    name = "celeris",
    about = "celeris daemon: deterministic task dispatcher (DESIGN.md §5.2)"
)]
struct Cli {
    /// 設定ファイル（TOML）。ADR-0045 D2: 既定は `~/.config/celeris/config.toml`
    /// （`~` は `$HOME` で展開する。`$HOME` が無い環境では `~/...` のまま渡って読めずに exit 2）。
    #[arg(long, default_value = DEFAULT_CONFIG)]
    config: PathBuf,
    /// 実行中・判定中・ready のタスクが無くなったら終了する。
    #[arg(long)]
    until_idle: bool,
    /// tick 数の上限（0 = 無制限）。
    #[arg(long, default_value_t = 0)]
    max_ticks: u64,
    /// ログ形式。
    #[arg(long, default_value = "json")]
    log_format: LogFormat,
    /// ADR-0040 D3: `verify` は本番のデータのコピーに対する検証専用（dispatch しない、ワーカーを
    /// 起こさない、tick の裏方を動かさない、Discord に送らない、`daemon_instances` に書かない）。
    #[arg(long, default_value = "normal")]
    mode: Mode,
    /// ADR-0040 D3: `db` の上書き（相対パスは設定ファイルのディレクトリ基準）。
    #[arg(long)]
    db: Option<PathBuf>,
    /// ADR-0040 D3: `[api] listen` の上書き（例 `127.0.0.1:7711`）。
    #[arg(long)]
    listen: Option<SocketAddr>,
    /// ADR-0040 D3: `workspace_root` の上書き（相対パスは設定ファイルのディレクトリ基準）。
    #[arg(long)]
    workspace_root: Option<PathBuf>,
    /// ADR-0040 D3: `[api] token_file` の上書き（相対パスは設定ファイルのディレクトリ基準）。
    #[arg(long)]
    token_file: Option<PathBuf>,
    /// ADR-0040 D4: このプロセスのリリース（`sha12`）。省略時は環境変数 `CELERIS_RELEASE`、
    /// それも無ければ `dev`。同じ `release` の `active` が既にいたら何もせず exit 3。
    #[arg(long)]
    release: Option<String>,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum Mode {
    Normal,
    Verify,
}

impl From<Mode> for task_core::DaemonMode {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Normal => Self::Normal,
            Mode::Verify => Self::Verify,
        }
    }
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum LogFormat {
    Json,
    Text,
}

fn init_tracing(format: LogFormat) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("CELERIS_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    match format {
        LogFormat::Json => builder.json().init(),
        LogFormat::Text => builder.init(),
    }
}

/// Phase 119 D1: `run()` が返った後、runtime の drop（≒ 未完了の `spawn_blocking` を待つ）に
/// 無期限に付き合わない上限。ここを超えたら諦めて先へ進む（それでも生きているブロッキングスレッドは
/// 直後の `std::process::exit` で OS が片付ける）。
const SHUTDOWN_BOUND: Duration = Duration::from_secs(10);

fn main() {
    let cli = Cli::parse();
    init_tracing(cli.log_format);
    // ADR-0045 D2: 既定値も人が書いた `--config ~/...` も同じ規則で展開する。
    let config_path = task_core::expand_home(&cli.config, task_core::home_dir().as_deref());
    let mut config = match Config::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };
    // ADR-0040 D3: 設定は本番のものをそのまま読み、**上書きは CLI だけ**。
    config.apply_overrides(&Overrides {
        db: cli.db,
        listen: cli.listen,
        workspace_root: cli.workspace_root,
        token_file: cli.token_file,
    });
    let opts = RunOptions {
        until_idle: cli.until_idle,
        max_ticks: cli.max_ticks,
        mode: cli.mode.into(),
        release: cli.release,
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to start the tokio runtime: {e}");
            std::process::exit(1);
        }
    };
    let code = runtime.block_on(run_and_report(config, opts));
    shutdown_and_exit(runtime, code);
}

/// `celeris::run` を実行し、終了コード（0/1/2/3。`main` が `std::process::exit` にそのまま渡す）に
/// まとめる。ログとメッセージの中身は従来と同じ。
async fn run_and_report(config: Config, opts: RunOptions) -> u8 {
    match celeris::run(config, opts).await {
        // ADR-0040 D4: 同じ版の `active` が既にいた。何も変えずに exit 3（promote.sh が見る）。
        Ok(celeris::Exit::DuplicateRelease) => {
            eprintln!("error: another instance of the same release is already active");
            3
        }
        Ok(exit) => {
            tracing::info!(?exit, "celeris stopped");
            0
        }
        Err(e) => {
            tracing::error!(error = %e, "celeris failed");
            eprintln!("error: {e}");
            // docs/gui/api.md §1.5: 知らない新しいスキーマ版数の DB は、設定エラーと同じく起動時の exit 2。
            if matches!(
                e,
                celeris::DaemonError::Store(task_core::StoreError::SchemaTooNew { .. })
            ) {
                2
            } else {
                1
            }
        }
    }
}

/// Phase 119 D1: `run()` が返した終了コードで**必ず**プロセスを終える。
///
/// `Runtime::shutdown_timeout` は待つ時間に上限があるので、`#[tokio::main]` の暗黙の `Drop`
/// （無期限に待つ）と違って、止まらない背景タスク（`spawn_blocking` の孤児、DB busy_timeout を
/// 使い切っていない最中のチェックポイント/バックアップ等）がいてもここで必ず先へ進む。それでも
/// 明示的に `std::process::exit` を呼ぶのは、`main` が普通に return したときの終了コードの扱いに
/// 賭けず、コードを読んだだけで「ここで確実にプロセスが終わる」と分かるようにするため。
///
/// `runtime` を先に drop してから `shutdown_and_exit` を呼ぶのではなく、ここで `shutdown_timeout` を
/// 明示的に呼ぶのがこの関数の核心: `Runtime` の暗黙の `Drop` は上限を持たないが、
/// `shutdown_timeout(bound)` は `bound` を超えたら（まだ動いているタスクがあっても）呼び出し元へ
/// 制御を返す。
fn shutdown_and_exit(runtime: tokio::runtime::Runtime, code: u8) -> ! {
    let started = std::time::Instant::now();
    runtime.shutdown_timeout(SHUTDOWN_BOUND);
    if started.elapsed() >= SHUTDOWN_BOUND {
        tracing::warn!(
            bound_secs = SHUTDOWN_BOUND.as_secs(),
            "runtime shutdown reached its time bound; a background task (checkpoint/backup/\
             tunnel-prober/spawn_blocking) likely did not stop cleanly; exiting the process anyway \
             (Phase 119 D1, ADR-0040 追記 / ADR-0064 追記)"
        );
    }
    // Phase 119 D2: runtime が完全に止まった後（＝ tokio 自身の SIGCHLD 駆動の reap と競合しない
    // ここでだけ）、`kill_on_drop` の非同期 reaper が shutdown に間に合わなかった子（ゾンビ）を拾う。
    let reaped = task_worker::process_group::reap_finished_children();
    if reaped > 0 {
        tracing::info!(
            reaped,
            "reaped zombie child processes before exiting (Phase 119 D2)"
        );
    }
    std::process::exit(code as i32);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D1（Phase 119）: 偽の「止まらない背景タスク」（stop シグナルを見ない、`spawn_blocking` の中で
    /// 長く眠り続ける）を持つ Runtime でも、`shutdown_and_exit` が使っている `shutdown_timeout` 相当の
    /// 呼び出しが**上限を超えて待たない**ことを確かめる。本番の事故（`celeris stopped` が journal に
    /// 出た後もプロセスが終了しない）は「`#[tokio::main]` の暗黙の `Drop` が無期限に待つ」ことが原因
    /// だったので、ここでは `Runtime::drop`（無期限）ではなく `shutdown_timeout(bound)`（有限）を
    /// 使えば、同じ状況でも制御が戻ってくることを示す。実プロセスを終わらせる部分
    /// （`std::process::exit`）はテストプロセスごと終わってしまうので確かめられないが、そこに至る前の
    /// `shutdown_timeout` が無期限に待たないことがこの Phase の core fix。
    #[test]
    fn shutdown_timeout_gives_up_within_its_bound_even_with_a_stuck_background_task() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap_or_else(|e| panic!("build runtime: {e}"));
        // 止まらない背景スレッドを模す: stop シグナルを一切見ない `spawn_blocking` の長い sleep
        // （Phase 110a のチェックポイント/バックアップや、その他の `spawn_blocking` がもし stop を
        // 見落としたらこうなる、という形そのもの）。
        runtime.spawn(async {
            let _ = tokio::task::spawn_blocking(|| {
                std::thread::sleep(Duration::from_secs(600));
            })
            .await;
        });
        // 上のタスクが実際に spawn_blocking のスレッドプールに乗るまでわずかに待つ。
        runtime.block_on(async { tokio::time::sleep(Duration::from_millis(50)).await });

        let bound = Duration::from_secs(3);
        let started = std::time::Instant::now();
        runtime.shutdown_timeout(bound);
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "shutdown_timeout should give up around its bound instead of waiting for the stuck \
             600s sleep, took {elapsed:?}"
        );
    }
}
