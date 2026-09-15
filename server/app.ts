import { createRequestHandler } from "@react-router/express";
import express from "express";
import { RouterContextProvider } from "react-router";

/** React Router のハンドラ（本番は build/server/index.js に入る。開発は Vite の ssrLoadModule で読む）。 */
export const app: express.Express = express();
app.disable("x-powered-by");

app.use(
  createRequestHandler({
    build: () => import("virtual:react-router/server-build"),
    getLoadContext() {
      return new RouterContextProvider();
    },
  }),
);
