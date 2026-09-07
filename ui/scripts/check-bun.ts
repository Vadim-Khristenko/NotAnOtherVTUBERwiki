// Fail fast when the runtime is older than the minimum the engine needs.
// Keep the range in sync with the "engines" field in package.json.
if (!Bun.semver.satisfies(Bun.version, ">=1.4.2")) {
  console.error(`naw-ui needs Bun >= 1.4.2, found ${Bun.version}`);
  process.exit(1);
}
