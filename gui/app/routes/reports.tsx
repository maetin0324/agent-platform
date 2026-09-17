import { PlaceholderPage } from "~/components/Placeholder";
import type { Route } from "./+types/reports";

/**
 * `/reports`（報告の流れ、SPEC §3.5・§4.4、ADR-0033 D3）。
 * 報告（`reports`）の生成・圧縮は taskd 側 Phase 25（D3）待ちなので、今回（G13a）はプレースホルダ。
 */
export function meta(_: Route.MetaArgs) {
  return [{ title: "報告 - taskd-gui" }];
}

export default function ReportsPlaceholder() {
  return (
    <PlaceholderPage
      icon="send"
      title="報告"
      description="各所から上がってくる報告を高速で流し見します。良い知らせも悪い知らせも。"
      specRef="SPEC §3.5"
      specQuote="上に行くほど多くのレビューが入り、圧縮される。だから上で見る報告はパッと見の判断が楽。通知は数分単位ではなく、数時間単位。"
    />
  );
}
