import { describe, expect, it, vi } from "vitest";
import { createConsoleStreamController, parseConsoleBlock, parseConsoleHello } from "~/hooks/useConsoleStream";

/**
 * `~/hooks/useConsoleStream.ts` の `EventSource` に依存しない部分（ADR-0048 D1、GUI Phase G22）。
 * `~/hooks/useCelerisStream.ts` の `createStreamController` と同じ理由でここだけ切り出してテストする
 * （React フックの配線〈EventSource の生成・再接続〉は DOM が要るので、このリポジトリの方針〈G10-U1〉どおり
 * DOM を描画する unit テストはしない）。
 */

describe("parseConsoleHello / parseConsoleBlock", () => {
  it("壊れた JSON は null を返す", () => {
    expect(parseConsoleHello("not json")).toBeNull();
    expect(parseConsoleBlock("{")).toBeNull();
  });

  it("正しい JSON はそのまま返す", () => {
    expect(parseConsoleHello('{"cursor":"c1","scope":"all","now":"t"}')).toEqual({
      cursor: "c1",
      scope: "all",
      now: "t",
    });
  });
});

describe("createConsoleStreamController", () => {
  it("hello でカーソルを覚え、onHello を呼ぶ", () => {
    const onBlock = vi.fn();
    const onHello = vi.fn();
    const controller = createConsoleStreamController({ onBlock, onHello });

    controller.handleHello('{"cursor":"c1","scope":"all","now":"t"}');

    expect(onHello).toHaveBeenCalledWith({ cursor: "c1", scope: "all", now: "t" });
    expect(controller.cursor()).toBe("c1");
    expect(onBlock).not.toHaveBeenCalled();
  });

  it("console.block でカーソルを更新し、onBlock を呼ぶ", () => {
    const onBlock = vi.fn();
    const controller = createConsoleStreamController({ onBlock }, "c0");

    controller.handleBlock('{"kind":"report","at":"t","cursor":"c1","report":{}}');

    expect(onBlock).toHaveBeenCalledTimes(1);
    expect(controller.cursor()).toBe("c1");
  });

  it("壊れたフレームは無視し、以後のフレームは受け取り続ける", () => {
    const onBlock = vi.fn();
    const controller = createConsoleStreamController({ onBlock }, "c0");

    controller.handleBlock("not json");
    expect(onBlock).not.toHaveBeenCalled();
    expect(controller.cursor()).toBe("c0");

    controller.handleBlock('{"kind":"report","at":"t","cursor":"c1","report":{}}');
    expect(onBlock).toHaveBeenCalledTimes(1);
    expect(controller.cursor()).toBe("c1");
  });

  it("初期カーソルを渡さなければ null から始まる", () => {
    const controller = createConsoleStreamController({ onBlock: vi.fn() });
    expect(controller.cursor()).toBeNull();
  });
});
