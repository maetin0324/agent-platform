import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

/** shadcn/ui の `cn`（クラス名の結合と Tailwind の重複解決）。部品は G1 以降で `shadcn add` する。 */
export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs));
}
