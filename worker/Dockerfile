FROM oven/bun:1.4.2
WORKDIR /app
# The lockfile glob tolerates its absence: there are no dependencies yet, but
# the moment Vue, rolldown or mermaid arrive (PLAN sections 2.4 and 5) this
# image has to install them, and a missing `bun install` would fail confusingly
# at runtime instead of at build time.
COPY package.json bun.lock* ./
RUN bun install --production
COPY src ./src
EXPOSE 8081
CMD ["bun", "run", "src/index.ts"]
