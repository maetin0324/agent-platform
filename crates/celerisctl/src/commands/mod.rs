pub mod add;
pub mod cancel;
/// ADR-0046 D3（Phase 59）: `celerisctl config to-harnesses`。
pub mod config;
/// ADR-0064 D2/D3（Phase 110a）: `celerisctl db backup|integrity-check`。DB を通常の経路では開かない。
pub mod db;
pub mod gate;
/// ADR-0047 D3（Phase 61）: 知識ベース（DB を開かない。`~/.local/share/celeris/knowledge` を直接読み書きする）。
pub mod knowledge;
/// ADR-0056 D1（Phase 78）: MCP クライアントの発行・一覧・失効、stdio 橋。
pub mod mcp;
/// ADR-0046 D7（Phase 59）: `celerisctl org migrate-v2`。
pub mod org;
pub mod plan;
/// ADR-0067 D5（Phase 111）: `celerisctl plan-lint`。draft/ready の受け入れ条件を D2 の規則で点検する
/// 読み取り専用コマンド。
pub mod plan_lint;
/// ADR-0054 D2（Phase 68）: `celerisctl projects ls|show`（CoS の対話 run に許す読み取りの道具）。
pub mod projects;
pub mod query;
pub mod replay;
/// ADR-0051 / ADR-0054 Phase 113 D3: `celerisctl rereview <task_id>`。既存成果を再判定する
/// （新しい実装 run は起こさない）。
pub mod rereview;
pub mod worker;
/// ADR-0066 D2（Phase 110b）: `celerisctl workspace prune`。
pub mod workspace;
