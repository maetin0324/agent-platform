# celeris-gui (docs/DESIGN.md §9)。docker build はこの開発環境では実行しない
# （registry へのアクセスが要るため。docs/adr/0008 D10）。
#
# 使い方（任意。§9「コンテナ」）:
#   docker build -t celeris-gui .
#   docker run --network host \
#     -e CELERIS_API_URL=http://127.0.0.1:7710 \
#     -e CELERIS_API_TOKEN_FILE=/run/secrets/celeris-api-token \
#     -v /path/to/token:/run/secrets/celeris-api-token:ro \
#     celeris-gui
# --network host を使わない場合は CELERIS_API_URL でホストの celeris を指す（例: http://host.docker.internal:7710）。

FROM node:24-slim AS builder
WORKDIR /app
RUN corepack enable
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY . .
RUN pnpm install --frozen-lockfile && pnpm build

FROM node:24-slim AS runner
WORKDIR /app
RUN corepack enable
COPY --from=builder /app/build ./build
COPY --from=builder /app/server.js ./server.js
COPY --from=builder /app/package.json ./package.json
COPY --from=builder /app/pnpm-lock.yaml ./pnpm-lock.yaml
COPY --from=builder /app/pnpm-workspace.yaml ./pnpm-workspace.yaml
RUN pnpm install --prod --frozen-lockfile --ignore-scripts

# コンテナ内でのバインドは常に非 loopback（0.0.0.0）扱いになるため、
# CELERIS_GUI_PASSWORD_FILE（と、必要なら CELERIS_GUI_SESSION_SECRET_FILE）を必ず渡すこと。
ENV CELERIS_GUI_BIND=0.0.0.0:7700
EXPOSE 7700

USER node
CMD ["node", "server.js"]
