import { PlaceholderPage } from "~/components/Placeholder";
import type { Route } from "./+types/artifacts";

/**
 * `/artifacts`（成果物、SPEC §3.7・§4.6）。
 * 調査文書・リンク集を読む場所は taskd 側の記憶（D6）・報告（D3）と合わせて G13b 以降で作るので、
 * 今回（G13a）はプレースホルダ。個々のタスクの成果物は従来どおり `/tasks/:id` で見られる。
 */
export function meta(_: Route.MetaArgs) {
  return [{ title: "成果物 - taskd-gui" }];
}

export default function ArtifactsPlaceholder() {
  return (
    <PlaceholderPage
      icon="file"
      title="成果物"
      description="調査文書・リンク集をここで読みます。コードは置き場所（~/workspace/…）へのリンクだけ。"
      specRef="SPEC §3.7"
      specQuote="人が普段見る場所に、普段の形で。コードは ~/workspace/… のリポジトリ、文書は GUI で読める形。基本は GUI から確認して楽をする。"
    />
  );
}
