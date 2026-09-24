//! `celerisctl mcp`（ADR-0056 D1。Phase 78）: MCP クライアントの発行・一覧・失効、stdio 橋。
//!
//! `client add|ls|revoke` は **DB を直接開く**（`knowledge rerun` / `org migrate-v2` と同じ管理系）。
//! `stdio` は DB を開かない（手元の HTTP に橋を架けるだけ。celeris が別プロセスで持つ）。
//!
//! トークンの値は `client add` が**発行時に 1 度だけ**標準出力に出す。DB にはハッシュしか残らない。

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand};
use task_core::{McpClient, McpClientStore, McpScope, SqliteStore};
use time::OffsetDateTime;

use crate::error::CliError;
use crate::outln;

#[derive(Subcommand, Debug)]
pub enum McpCommand {
    /// クライアント（`mcp_clients`）の発行・一覧・失効。
    Client {
        #[command(subcommand)]
        command: ClientCommand,
    },
    /// stdio ↔ 手元の HTTP（`POST <base-url>/mcp`）の橋。**DB は開かない**。
    Stdio(StdioArgs),
    /// ADR-0056 Phase 101: `initialize` → `tools/call`（または `--list` で `tools/list`）を 1 回
    /// だけ行う。**DB は開かない**（`stdio` と同じ HTTP クライアントを再利用する）。
    /// `scripts/rdc/celeris-chat` から呼ばれる想定（RDC 経由の ChatGPT 向け）。
    Call(CallArgs),
}

#[derive(Subcommand, Debug)]
pub enum ClientCommand {
    /// 新しいクライアントを作る（トークンは発行時に 1 度だけ表示。DB にはハッシュのみ）。
    Add(ClientAddArgs),
    /// 一覧（id / name / scopes / last_used_at。トークンは出ない）。
    Ls,
    /// 失効させる（以後そのトークンは使えない）。
    Revoke(ClientRevokeArgs),
}

#[derive(Args, Debug)]
pub struct ClientAddArgs {
    /// 人が付ける名前（`chatgpt` 等）。
    pub name: String,
    /// `knowledge:read` など（カンマ区切り、複数回も可）。省略時は既定
    /// （read 系 + `knowledge:propose` + `console:instruct`。ADR-0056 D4）。
    #[arg(long = "scope", value_delimiter = ',')]
    pub scopes: Vec<String>,
    /// トークンを発行しない（`auth = "none"` の口に `client = "<id>"` で固定する専用）。
    #[arg(long)]
    pub no_token: bool,
}

#[derive(Args, Debug)]
pub struct ClientRevokeArgs {
    pub id: String,
}

#[derive(Args, Debug)]
pub struct StdioArgs {
    /// `celeris` の MCP の口（既定は `[mcp] listen` の既定と同じ）。
    #[arg(long = "base-url", default_value = "http://127.0.0.1:18200")]
    pub base_url: String,
    /// トークンの入ったファイル（1 行目を使う）。省略時は環境変数 `CELERIS_MCP_TOKEN`。
    /// `auth = "none"` の口を橋渡しするだけなら、どちらも省略してよい。
    #[arg(long = "token-file")]
    pub token_file: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct CallArgs {
    /// 呼ぶ道具の名前（`tools_list` 等。`--list` のときは省略できる）。
    pub tool: Option<String>,
    /// 道具への引数（JSON オブジェクトの文字列）。省略時は `{}`。
    pub json_args: Option<String>,
    /// `celeris` の MCP の口（既定は `[mcp] listen` の既定と同じ）。
    #[arg(long = "base-url", default_value = "http://127.0.0.1:18200")]
    pub base_url: String,
    /// トークンの入ったファイル（1 行目を使う）。省略時は環境変数 `CELERIS_MCP_TOKEN`。
    #[arg(long = "token-file")]
    pub token_file: Option<PathBuf>,
    /// `tool`/`json_args` の代わりに `tools/list` の名前と description を出す。
    #[arg(long)]
    pub list: bool,
}

/// `StdioArgs`/`CallArgs` 共通: `--token-file` の 1 行目、省略時は `CELERIS_MCP_TOKEN`。
fn resolve_token(token_file: Option<&PathBuf>) -> Result<Option<String>, CliError> {
    match token_file {
        Some(path) => {
            let mut raw = String::new();
            std::fs::File::open(path)
                .map_err(|e| CliError::msg(format!("{}: {e}", path.display())))?
                .read_to_string(&mut raw)
                .map_err(|e| CliError::msg(format!("{}: {e}", path.display())))?;
            Ok(Some(
                raw.lines().next().unwrap_or_default().trim().to_string(),
            ))
        }
        None => Ok(std::env::var("CELERIS_MCP_TOKEN").ok()),
    }
}

/// `mcp client …`（DB を直接開く。呼び出し側 = `main` がその前提で store を渡す）。
pub fn run_client(store: &SqliteStore, command: ClientCommand) -> Result<ExitCode, CliError> {
    match command {
        ClientCommand::Add(args) => run_add(store, args),
        ClientCommand::Ls => run_ls(store),
        ClientCommand::Revoke(args) => run_revoke(store, args),
    }
}

fn parse_scopes(raw: &[String]) -> Result<Vec<McpScope>, CliError> {
    if raw.is_empty() {
        return Ok(McpScope::DEFAULT.to_vec());
    }
    raw.iter()
        .map(|s| {
            McpScope::parse(s.trim())
                .ok_or_else(|| CliError::msg(format!("unknown scope {s:?} (see docs/mcp.md)")))
        })
        .collect()
}

fn run_add(store: &SqliteStore, args: ClientAddArgs) -> Result<ExitCode, CliError> {
    if args.name.trim().is_empty() {
        return Err(CliError::msg("name must not be blank"));
    }
    let scopes = parse_scopes(&args.scopes)?;
    let now = OffsetDateTime::now_utc();
    // ADR-0056 D1 の `[[mcp.listeners]] client = "<id>"` の例（`client = "chatgpt"`）は
    // `celerisctl mcp client add chatgpt` の `<name>` をそのまま指す。`add` の引数が名前 1 つだけ
    // （別に id を選ばせる引数が無い）ことから、CLI で作るクライアントは **id = name**（`mcp_clients.id`
    // は主キーなので、既に同じ名前があれば友好的なエラーにする）。
    let id = args.name.trim().to_string();
    if store.mcp_client_get(&id)?.is_some() {
        return Err(CliError::msg(format!(
            "an mcp client named {id:?} already exists (names must be unique; they double as the id)"
        )));
    }
    let token = if args.no_token {
        None
    } else {
        Some(celeris_mcp::auth::generate_token())
    };
    let token_hash = token.as_deref().map(celeris_mcp::auth::hash_token);
    let client = McpClient {
        id: id.clone(),
        name: args.name.clone(),
        token_hash,
        scopes,
        created_at: now,
        last_used_at: None,
        revoked_at: None,
    };
    store.mcp_client_create(&client)?;
    outln!("id: {id}");
    outln!("name: {}", args.name);
    outln!("scopes: {}", task_core::scopes_to_string(&client.scopes));
    match token {
        Some(token) => {
            outln!("token: {token}");
            outln!(
                "(この値は 2 度と表示されません。DB にはハッシュしか残りません。安全な場所に控えてください)"
            );
        }
        None => {
            outln!(
                "token: (無し。auth = \"none\" の口に client = {id:?} で固定して使ってください)"
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn run_ls(store: &SqliteStore) -> Result<ExitCode, CliError> {
    let clients = store.mcp_client_list()?;
    if clients.is_empty() {
        outln!("(no mcp clients)");
        return Ok(ExitCode::SUCCESS);
    }
    for c in clients {
        let scopes = task_core::scopes_to_string(&c.scopes);
        let last_used = c
            .last_used_at
            .map(|t| t.to_string())
            .unwrap_or_else(|| "-".to_string());
        let revoked = if c.revoked_at.is_some() {
            " revoked"
        } else {
            ""
        };
        outln!(
            "{}\t{}\t{}\tlast_used={}{}",
            c.id,
            c.name,
            scopes,
            last_used,
            revoked
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn run_revoke(store: &SqliteStore, args: ClientRevokeArgs) -> Result<ExitCode, CliError> {
    let now = OffsetDateTime::now_utc();
    if store.mcp_client_revoke(&args.id, now)? {
        outln!("revoked: {}", args.id);
        Ok(ExitCode::SUCCESS)
    } else {
        Err(CliError::msg(format!(
            "{:?} is not a known, still-active mcp client",
            args.id
        )))
    }
}

/// `mcp stdio`（DB は開かない）。
pub fn run_stdio(args: StdioArgs) -> Result<ExitCode, CliError> {
    let token = resolve_token(args.token_file.as_ref())?;
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    celeris_mcp::stdio::run(
        &args.base_url,
        token.as_deref(),
        stdin.lock(),
        stdout.lock(),
    )
    .map_err(CliError::msg)?;
    let _ = std::io::stdout().flush();
    Ok(ExitCode::SUCCESS)
}

/// `mcp call`（DB は開かない。ADR-0056 Phase 101）。引数不正は exit code 2、
/// サーバー側のエラー（JSON-RPC error / HTTP 401 等 / 接続不可）は 1（`CliError` 経由で
/// `main` が `eprintln!("error: {e}")` してから `ExitCode::FAILURE` = 1 を返す）。
pub fn run_call(args: CallArgs) -> Result<ExitCode, CliError> {
    let token = resolve_token(args.token_file.as_ref())?;

    if args.list {
        let tools = celeris_mcp::call::list_tools(&args.base_url, token.as_deref())
            .map_err(|e| CliError::msg(e.to_string()))?;
        for t in tools {
            outln!("{}\t{}", t.name, t.description);
        }
        return Ok(ExitCode::SUCCESS);
    }

    let Some(tool) = args.tool else {
        eprintln!("error: TOOL is required (or pass --list)");
        return Ok(ExitCode::from(2));
    };
    let raw_args = args.json_args.as_deref().unwrap_or("{}");
    let arguments: serde_json::Value = match serde_json::from_str(raw_args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: JSON_ARGS is not valid JSON: {e}");
            return Ok(ExitCode::from(2));
        }
    };
    let out = celeris_mcp::call::call_tool(&args.base_url, token.as_deref(), &tool, arguments)
        .map_err(|e| CliError::msg(e.to_string()))?;
    outln!("{out}");
    Ok(ExitCode::SUCCESS)
}
