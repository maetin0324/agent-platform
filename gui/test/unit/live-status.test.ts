import { describe, expect, it } from "vitest";
import { isLiveStatusScreen } from "~/lib/live-status";

// U-G32-2 の解消（Phase 84）: role="status" を board/task の状態バッジにだけ広げる判断を集約した
// 純粋関数。SSE の再検証で更新され続ける画面（board/task-list/task-detail）だけが true。
describe("isLiveStatusScreen", () => {
  it.each<[Parameters<typeof isLiveStatusScreen>[0], boolean]>([
    ["board", true],
    ["task-list", true],
    ["task-detail", true],
    ["task-new-candidates", false],
    ["project-integrations", false],
    ["help-example", false],
  ])("isLiveStatusScreen(%s) === %s", (screen, expected) => {
    expect(isLiveStatusScreen(screen)).toBe(expected);
  });
});
