/**
 * 成果物・ログの本体をどのビューアで表示するかを決める純粋関数。
 * docs/adr/0006 D3: 選択は `Content-Type` と名前だけで行い、taskd 側の値をそのまま使う（GUI で再判定・再計算しない）。
 */

export type ViewerKind = "code" | "markdown" | "image";

const IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/** `contentType` に応じてビューアの種類を選ぶ。`name` は将来の拡張用に受け取るのみで現在は未使用。 */
export function pickViewer(contentType: string, name: string): ViewerKind {
  void name;
  if (contentType.includes("text/markdown")) return "markdown";
  if (IMAGE_TYPES.some((t) => contentType.includes(t))) return "image";
  return "code";
}

/** `CodeViewer` が JSON の構文強調を使うかどうかの判定。 */
export function isJson(contentType: string): boolean {
  return contentType.includes("application/json");
}

/**
 * 成果物一覧の 1 件に添える状態メッセージ（`null` なら何も出さない）。
 * `ArtifactView.forbidden` / `exists` は taskd が計算済みの値をそのまま使う（GUI で再判定しない）。
 * 文言をここに集約することで、DOM 描画ライブラリ無しでも（`test/unit/artifact-view.test.ts`）表示文言を検証できる
 * （docs/DESIGN.md §10 Phase G3 受け入れ条件 5。本体の 403 中継は `test/unit/files.route.test.ts` で別途確認）。
 */
export function artifactStatusMessage(artifact: { exists: boolean; forbidden: boolean }): string | null {
  if (artifact.forbidden) return "アクセスできません（path_forbidden）";
  if (!artifact.exists) return "ファイルがありません。";
  return null;
}
