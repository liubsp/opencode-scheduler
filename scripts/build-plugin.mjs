import { createRequire, isBuiltin } from "node:module";
import { mkdir, rm, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import { nodeNotices } from "./notices.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
const require = createRequire(join(root, "plugin/package.json"));
const { build } = require("esbuild");
await rm(join(root, "plugin/dist"), { recursive: true, force: true });
const result = await build({
  absWorkingDir: root,
  entryPoints: ["plugin/src/index.ts"],
  outfile: "plugin/dist/index.js",
  bundle: true,
  platform: "node",
  target: "node22",
  format: "esm",
  sourcemap: false,
  metafile: true,
  legalComments: "inline",
});
for (const output of Object.values(result.metafile.outputs)) {
  for (const entry of output.imports) {
    if (!entry.external || !isBuiltin(entry.path))
      throw new Error(`Unreviewed runtime import: ${entry.path}`);
  }
}
await mkdir(join(root, ".tmp/release"), { recursive: true });
await writeFile(
  join(root, ".tmp/release/plugin-meta.json"),
  JSON.stringify(result.metafile, null, 2),
);
await writeFile(
  join(root, "plugin/THIRD-PARTY-NOTICES.txt"),
  await nodeNotices(root, result.metafile),
);
console.log("Built standalone ESM plugin (Node builtins only).");
