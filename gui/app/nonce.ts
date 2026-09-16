import { createContext, useContext } from "react";

/** React 側で nonce を配る context（サーバ描画時だけ値が入る。クライアントでは undefined）。 */
export const NonceContext = createContext<string | undefined>(undefined);

export function useNonce(): string | undefined {
  return useContext(NonceContext);
}
