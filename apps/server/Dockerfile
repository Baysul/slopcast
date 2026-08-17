# syntax=docker/dockerfile:1.7
FROM node:24-slim AS builder
ENV PNPM_HOME="/pnpm"
ENV PATH="$PNPM_HOME:$PATH"
RUN corepack enable
WORKDIR /app
COPY pnpm-lock.yaml pnpm-workspace.yaml ./
COPY package.json ./
COPY apps/server/package.json apps/server/
COPY packages/shared-types/package.json packages/shared-types/
RUN --mount=type=cache,id=pnpm,target=/pnpm/store pnpm install --frozen-lockfile
COPY packages/shared-types/ packages/shared-types/
COPY apps/server/ apps/server/
RUN --mount=type=cache,id=pnpm,target=/pnpm/store pnpm --filter @slopcast/shared-types build
RUN --mount=type=cache,id=pnpm,target=/pnpm/store pnpm --filter server build
RUN pnpm --filter server --prod deploy --legacy /deploy

FROM node:24-slim
ENV PNPM_HOME="/pnpm"
ENV PATH="$PNPM_HOME:$PATH"
RUN corepack enable
COPY --from=builder /deploy /app
WORKDIR /app
ENV SERVER_PORT=3001
EXPOSE 3001
USER node
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD node -e "fetch('http://localhost:' + (process.env.SERVER_PORT || '3001') + '/health').then(r => r.ok ? process.exit(0) : process.exit(1)).catch(() => process.exit(1))"
CMD ["node", "dist/index.js"]
