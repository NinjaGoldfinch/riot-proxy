# syntax=docker/dockerfile:1

# ── build ────────────────────────────────────────────────────────────────────
FROM node:26-alpine AS build
WORKDIR /app

# .npmrc carries engine-strict, so a base image on the wrong Node fails here
# rather than at some confusing point later in the build.
COPY package.json package-lock.json .npmrc ./
RUN npm ci

COPY tsconfig.json tsconfig.build.json ./
COPY src ./src
# Not compiled, just served — the dev UI is off by default in production, but
# an image where turning DEV_UI on 404s would be a worse kind of surprise.
COPY public ./public
RUN npm run build

# Drop dev dependencies from the tree we are about to copy forward.
RUN npm prune --omit=dev

# ── runtime ──────────────────────────────────────────────────────────────────
FROM node:26-alpine AS runtime
WORKDIR /app
ENV NODE_ENV=production

# Migrations are read from disk at runtime by drizzle's migrator.
COPY --from=build /app/node_modules ./node_modules
COPY --from=build /app/dist ./dist
COPY --from=build /app/package.json ./package.json
COPY --from=build /app/public ./public
COPY src/db/migrations ./dist/db/migrations

RUN mkdir -p /data/ddragon && chown -R node:node /data /app
USER node

EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
  CMD node -e "fetch('http://127.0.0.1:'+(process.env.PORT||8080)+'/healthz').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))"

CMD ["node", "dist/index.js"]
