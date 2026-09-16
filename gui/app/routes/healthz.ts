import { version } from "../../package.json";

/** GUI 自身の生存確認（docs/DESIGN.md §6.2）。taskd は呼ばない。 */
export function loader() {
  return Response.json({ ok: true, name: "taskd-gui", version }, { headers: { "Cache-Control": "no-store" } });
}
