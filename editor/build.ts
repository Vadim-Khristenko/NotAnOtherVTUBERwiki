// Builds the rich editor into the default skin, where the engine serves it
// with a content hash. usage: bun run build
const result = await Bun.build({
  entrypoints: ["./src/main.ts"],
  outdir: "../skins/default/scripts",
  naming: "editor.js",
  target: "browser",
  format: "esm",
  minify: true,
  sourcemap: "none",
});
if (!result.success) {
  for (const log of result.logs) console.error(log);
  process.exit(1);
}
for (const out of result.outputs) {
  console.log(`${out.path}  ${(out.size / 1024).toFixed(1)} KiB`);
}
