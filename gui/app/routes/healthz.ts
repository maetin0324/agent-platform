import { version } from "../../package.json";

/**
 * GUI 自身の生存確認（docs/DESIGN.md §6.2）。taskd は呼ばない。認証の対象外（app/auth.server.ts の
 * PUBLIC_PATHS）なので、昇格の途中でも外から読める。
 *
 * ADR-0040 D4（Phase 46）: `release` を足した。`TASKD_GUI_RELEASE`（systemd の `taskd-gui@<sha12>` が
 * `%i` を渡す）が無ければ `"dev"`。promote.sh は :7700 の `/healthz` がこの値を新しい sha12 に
 * 変えたのを見てから旧 GUI を止める。
 */
export function loader() {
  return Response.json(
    { ok: true, name: "taskd-gui", version, release: process.env.TASKD_GUI_RELEASE ?? "dev" },
    { headers: { "Cache-Control": "no-store" } },
  );
}
