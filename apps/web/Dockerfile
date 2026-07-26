# syntax=docker/dockerfile:1.7
FROM node:24-slim AS builder
ENV PNPM_HOME="/pnpm"
ENV PATH="$PNPM_HOME:$PATH"
RUN corepack enable
WORKDIR /app
COPY pnpm-lock.yaml pnpm-workspace.yaml ./
COPY package.json ./
COPY apps/web/package.json apps/web/
COPY packages/shared-types/package.json packages/shared-types/
RUN --mount=type=cache,id=pnpm,target=/pnpm/store pnpm install --frozen-lockfile
COPY packages/shared-types/ packages/shared-types/
COPY apps/web/ apps/web/
RUN --mount=type=cache,id=pnpm,target=/pnpm/store pnpm --filter @slopcast/shared-types build
RUN --mount=type=cache,id=pnpm,target=/pnpm/store pnpm --filter web build

FROM node:24-slim
ENV PNPM_HOME="/pnpm"
ENV PATH="$PNPM_HOME:$PATH"
RUN corepack enable && npm install -g serve
COPY --from=builder /app/apps/web/dist /app
COPY apps/web/docker-entrypoint.sh /docker-entrypoint.sh
RUN chmod +x /docker-entrypoint.sh
RUN chown -R node:node /app
ENV API_ENDPOINT=http://localhost:3001
ENV WEB_PORT=3000
EXPOSE 3000
USER node
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD node -e "fetch('http://localhost:' + (process.env.WEB_PORT || '3000') + '/').then(r => r.ok ? process.exit(0) : process.exit(1)).catch(() => process.exit(1))"
ENTRYPOINT ["/docker-entrypoint.sh"]
