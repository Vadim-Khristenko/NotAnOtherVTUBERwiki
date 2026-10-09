FROM oven/bun:1.4.2
WORKDIR /app
# Chromium lays out mermaid diagrams; fonts give its text real metrics.
RUN apt-get update  && apt-get install -y --no-install-recommends chromium fonts-dejavu-core  && rm -rf /var/lib/apt/lists/*
ENV CHROME_PATH=/usr/bin/chromium
COPY package.json bun.lock ./
RUN bun install --production --frozen-lockfile
COPY src ./src
EXPOSE 8081
CMD ["bun", "run", "src/index.ts"]
