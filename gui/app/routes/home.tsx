import { redirect } from "react-router";
import type { Route } from "./+types/home";

/**
 * `/`（最初の画面）。Phase G13f-1（監査 2）で**秘書**（`/org/secretary`）にした。
 * SPEC §4 の 1「秘書との対話 — 案件を投げる、状況を聞く、方針を変える」が人の入口で、
 * 受信箱（裏方の語彙: 承認待ち・draft・注意）は裏方の区画（`/inbox`）に残す。
 *
 * taskd には問い合わせない（リダイレクトするだけ。認証と Host 検査は root の middleware が済ませている）。
 */
export function loader(_: Route.LoaderArgs) {
  return redirect("/org/secretary");
}

export default function HomePage() {
  return null;
}
