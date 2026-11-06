import assert from "node:assert/strict";
import { test } from "node:test";
import { execFileSync } from "node:child_process";
import { readFile, symlink } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { fixture } from "./fixture.js";

test("packed built ESM entrypoint imports under plain Node without private config or source maps", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  const pluginRoot = fileURLToPath(new URL("../", import.meta.url));
  assert.ok(process.env.npm_execpath, "Run package tests through npm test");
  const metadata = JSON.parse(
    execFileSync(
      process.execPath,
      [
        process.env.npm_execpath!,
        "pack",
        "--ignore-scripts",
        "--json",
        "--pack-destination",
        f.home,
      ],
      { cwd: pluginRoot, encoding: "utf8", windowsHide: true },
    ),
  )[0];
  assert.deepEqual(metadata.files.map((f: { path: string }) => f.path).sort(), [
    "LICENSE",
    "dist/client.js",
    "dist/index.js",
    "package.json",
  ]);
  execFileSync("tar", ["-xzf", join(f.home, metadata.filename), "-C", f.home], {
    windowsHide: true,
  });
  await symlink(
    join(pluginRoot, "node_modules"),
    join(f.home, "node_modules"),
    "junction",
  );
  const output = execFileSync(
    process.execPath,
    [
      "--input-type=module",
      "-e",
      "import {pathToFileURL} from 'node:url'; const {default:p}=await import(pathToFileURL(process.argv[1])); if(typeof p.setup!=='function')throw new Error('No plugin setup'); console.log(p.id)",
      join(f.home, "package/dist/index.js"),
    ],
    { encoding: "utf8", windowsHide: true },
  );
  assert.equal(output.trim(), "opencode-scheduler");
  for (const { path } of metadata.files) {
    const body = await readFile(join(f.home, "package", path), "utf8");
    assert.ok(!body.includes(pluginRoot));
    assert.ok(!body.includes(f.token));
    assert.ok(!/sourceMappingURL|[A-Za-z]:\\(?:Users|Dev)\\/.test(body));
  }
});
