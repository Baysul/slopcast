FROM node:24-alpine AS builder
RUN corepack enable
WORKDIR /app
COPY pnpm-lock.yaml pnpm-workspace.yaml ./
COPY package.json ./
COPY apps/server/package.json apps/server/
COPY packages/shared-types/package.json packages/shared-types/
RUN pnpm install --frozen-lockfile
COPY packages/shared-types/ packages/shared-types/
COPY apps/server/ apps/server/
RUN pnpm --filter @slopcast/shared-types build
RUN pnpm --filter server build
RUN pnpm --filter server deploy --prod /deploy

FROM node:24-alpine
COPY --from=builder /deploy /app
WORKDIR /app
ENV SERVER_PORT=3001
EXPOSE 3001
CMD ["node", "dist/index.js"]
