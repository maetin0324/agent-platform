import { describe, expect, it } from "vitest";
import { artifactStatusMessage, isJson, pickViewer } from "~/lib/artifact-view";

describe("pickViewer", () => {
  it("markdown", () => {
    expect(pickViewer("text/markdown; charset=utf-8", "note.md")).toBe("markdown");
  });

  it("image", () => {
    expect(pickViewer("image/png", "image.png")).toBe("image");
  });

  it("json falls back to code", () => {
    expect(pickViewer("application/json", "data.json")).toBe("code");
  });

  it("plain text falls back to code", () => {
    expect(pickViewer("text/plain; charset=utf-8", "stderr.log")).toBe("code");
  });
});

describe("isJson", () => {
  it("true for application/json", () => {
    expect(isJson("application/json")).toBe(true);
  });

  it("false for text/plain", () => {
    expect(isJson("text/plain")).toBe(false);
  });
});

// docs/DESIGN.md §10 Phase G3 受け入れ条件 5: mock-celeris が 403 path_forbidden 相当（ArtifactList の
// forbidden:true）を返したとき、画面が「アクセスできません（path_forbidden）」を表示する文言の出所。
describe("artifactStatusMessage", () => {
  it("forbidden な成果物は「アクセスできません（path_forbidden）」", () => {
    expect(artifactStatusMessage({ exists: false, forbidden: true })).toBe("アクセスできません（path_forbidden）");
  });

  it("forbidden ではないが存在しない成果物は「ファイルがありません。」", () => {
    expect(artifactStatusMessage({ exists: false, forbidden: false })).toBe("ファイルがありません。");
  });

  it("存在して forbidden でもない成果物は null（何も表示しない）", () => {
    expect(artifactStatusMessage({ exists: true, forbidden: false })).toBeNull();
  });
});
