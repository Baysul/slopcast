FROM node:24-alpine AS builder
RUN corepack enable
WORKDIR /app
COPY pnpm-lock.yaml pnpm-workspace.yaml ./
COPY package.json ./
COPY apps/web/package.json apps/web/
COPY packages/shared-types/package.json packages/shared-types/
RUN pnpm install --frozen-lockfile
COPY packages/shared-types/ packages/shared-types/
COPY apps/web/ apps/web/
RUN pnpm --filter @slopcast/shared-types build
RUN pnpm --filter web build

FROM node:24-alpine
RUN npm install -g serve
COPY --from=builder /app/apps/web/dist /app
COPY apps/web/docker-entrypoint.sh /docker-entrypoint.sh
RUN chmod +x /docker-entrypoint.sh
ENV API_ENDPOINT=http://localhost:3001
ENV WEB_PORT=3000
EXPOSE 3000
ENTRYPOINT ["/docker-entrypoint.sh"]
