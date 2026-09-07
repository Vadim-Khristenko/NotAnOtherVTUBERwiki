FROM oven/bun:1.4.2
WORKDIR /app
COPY package.json ./
COPY src ./src
EXPOSE 8081
CMD ["bun", "run", "src/index.ts"]
