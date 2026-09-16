import { createContext } from "react-router";

/** CSP の nonce。要求ごとに security middleware が生成し、entry.server が <Scripts nonce> と React の inline script に渡す。 */
export const nonceContext = createContext<string>("");
