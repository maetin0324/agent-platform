import { PlaceholderPage } from "~/components/Placeholder";
import type { Route } from "./+types/org.secretary";

/**
 * `/org/secretary`（秘書との対話、SPEC §3.1・§4.1、ADR-0033 D4）。
 * 対話（`messages` / `conversations`）は taskd 側 Phase 24（D4）待ちなので、今回（G13a）はプレースホルダ。
 */
export function meta(_: Route.MetaArgs) {
  return [{ title: "秘書 - taskd-gui" }];
}

export default function SecretaryPlaceholder() {
  return (
    <PlaceholderPage
      icon="message"
      title="秘書"
      description="あなたの相手。案件を受け取り、組織に流し、報告を集めてあなたに渡します。"
      specRef="SPEC §3.1"
      specQuote="あなたの相手。案件を受け取り、組織に流し、報告を集めてあなたに渡す。部をまたぐ連携は秘書が認める。"
    />
  );
}
