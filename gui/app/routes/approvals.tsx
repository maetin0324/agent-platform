import { PlaceholderPage } from "~/components/Placeholder";
import type { Route } from "./+types/approvals";

/**
 * `/approvals`（認可の要求、SPEC §3.6・§4.5、ADR-0033 D5）。
 * 認可（`approvals` / `standing_rules`）は taskd 側 Phase 26（D5）待ちなので、今回（G13a）はプレースホルダ。
 */
export function meta(_: Route.MetaArgs) {
  return [{ title: "認可 - taskd-gui" }];
}

export default function ApprovalsPlaceholder() {
  return (
    <PlaceholderPage
      icon="shield"
      title="認可"
      description="聞かれたことに「今回だけ／今後ずっと」で答えます。永続の認可の一覧と編集もここで行います。"
      specRef="SPEC §3.6"
      specQuote="少しでも聞くべきだとエージェントが判断したら、あなたに指示を仰ぐ。あなたはそれに対して「今回だけ」か「同じようなことは今後ずっと」のどちらかの認可を出す。"
    />
  );
}
