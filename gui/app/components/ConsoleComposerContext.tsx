import { createContext, type ReactNode, useContext, useEffect, useMemo, useState } from "react";
import type { OrgNode, Project } from "~/celeris/types";
import type { InstructReplyTarget } from "~/lib/console";

/**
 * Console composer（入力欄）をレイアウトレベル（`~/root.tsx`）へ出すための登録 API（ADR-0057）。
 *
 * `Console`（`~/components/Console.tsx`、`/` と `/org/:id` の両方から使われる）が持っていたデータ
 * （`org`/`projects`/`streaming`）と、ブロックの「返信」で決まる `replyTarget` をここに集める。
 * `~/root.tsx` はこの Context を読むだけで、celeris を直接呼ばない（GUI の境界はそのまま）。
 *
 * `~/components/ConsoleComposer.tsx` がこの Context を読んで実際の入力欄を描く。root（モバイル用の
 * 固定表示）と `Console`（デスクトップ用の静的配置）の 2 か所から呼ばれ、どちらも同じ状態を見る。
 */

export interface ConsoleComposerRegistration {
  org: readonly OrgNode[];
  projects: readonly Project[];
  /** いま育っている返事があるか（ADR-0054 D2 のキュー）。`~/lib/console.ts::hasStreamingReply`。 */
  streaming: boolean;
}

const EMPTY_REGISTRATION: ConsoleComposerRegistration = { org: [], projects: [], streaming: false };

interface ConsoleComposerContextValue {
  registration: ConsoleComposerRegistration;
  replyTarget: InstructReplyTarget | null;
  setReplyTarget: (target: InstructReplyTarget | null) => void;
  /** モバイル版 composer（root 直下、`position: fixed`）の実高さ。Console 側の spacer がこれを読む。 */
  mobileHeight: number | null;
  setMobileHeight: (height: number) => void;
  /** @internal `useRegisterConsoleComposer`専用。直接は呼ばない。 */
  _setRegistration: (registration: ConsoleComposerRegistration | null) => void;
}

const ConsoleComposerContext = createContext<ConsoleComposerContextValue | null>(null);

export function ConsoleComposerProvider({ children }: { children: ReactNode }) {
  const [registration, setRegistrationState] = useState<ConsoleComposerRegistration>(EMPTY_REGISTRATION);
  const [replyTarget, setReplyTarget] = useState<InstructReplyTarget | null>(null);
  const [mobileHeight, setMobileHeight] = useState<number | null>(null);

  const value = useMemo<ConsoleComposerContextValue>(
    () => ({
      registration,
      replyTarget,
      setReplyTarget,
      mobileHeight,
      setMobileHeight,
      _setRegistration: (next) => setRegistrationState(next ?? EMPTY_REGISTRATION),
    }),
    [registration, replyTarget, mobileHeight],
  );

  return <ConsoleComposerContext.Provider value={value}>{children}</ConsoleComposerContext.Provider>;
}

function useConsoleComposerContextValue(): ConsoleComposerContextValue {
  const ctx = useContext(ConsoleComposerContext);
  if (!ctx) throw new Error("useConsoleComposerContext must be used within ConsoleComposerProvider");
  return ctx;
}

/** `~/components/ConsoleComposer.tsx` が読む（登録 API 以外の全部）。 */
export function useConsoleComposerContext() {
  const { registration, replyTarget, setReplyTarget, mobileHeight, setMobileHeight } = useConsoleComposerContextValue();
  return { registration, replyTarget, setReplyTarget, mobileHeight, setMobileHeight };
}

/**
 * `Console`（`/` と `/org/:id`）が呼ぶ登録 API（P-G42-1）。マウント中は最新の `registration` を反映し、
 * アンマウント時（画面を離れた）は既定値に戻し、返信先も一緒に捨てる（「ページを離れたら破棄」）。
 * `registration` は呼び出し側で値が変わったときだけ新しい参照になるよう `useMemo` すること
 * （さもないと毎レンダー unmount 相当のクリーンアップが走り、返信先が消えてしまう）。
 */
export function useRegisterConsoleComposer(registration: ConsoleComposerRegistration): void {
  const { _setRegistration, setReplyTarget } = useConsoleComposerContextValue();

  useEffect(() => {
    _setRegistration(registration);
  }, [registration, _setRegistration]);

  // 本当にアンマウント時だけ走らせたい（マウント中の registration 更新では走らせない）。
  // `_setRegistration`/`setReplyTarget` は Provider が useMemo で安定させている。
  // biome-ignore lint/correctness/useExhaustiveDependencies: 上記の理由で意図的に空配列にしている。
  useEffect(() => {
    return () => {
      _setRegistration(null);
      setReplyTarget(null);
    };
  }, []);
}
