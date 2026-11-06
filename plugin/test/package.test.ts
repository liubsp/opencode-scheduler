import assert from "node:assert/strict";
import { test } from "node:test";
import { execFile, execFileSync } from "node:child_process";
import { copyFile, mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { promisify } from "node:util";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { fixture } from "./fixture.js";

test("packed built ESM entrypoint imports under plain Node without private config or source maps", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  const outside = await mkdtemp(join(tmpdir(), "scheduler-packed-"));
  t.after(() => rm(outside, { recursive: true, force: true }));
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
        outside,
      ],
      { cwd: pluginRoot, encoding: "utf8", windowsHide: true },
    ),
  )[0];
  assert.deepEqual(metadata.files.map((f: { path: string }) => f.path).sort(), [
    "LICENSE",
    "THIRD-PARTY-NOTICES.txt",
    "dist/index.js",
    "package.json",
  ]);
  execFileSync(
    "tar",
    ["-xzf", join(outside, metadata.filename), "-C", outside],
    {
      windowsHide: true,
    },
  );
  const pkg = JSON.parse(
    await readFile(join(outside, "package/package.json"), "utf8"),
  );
  assert.ok(!pkg.dependencies || Object.keys(pkg.dependencies).length === 0);
  await copyFile(f.configPath, join(outside, "package/install.json"));
  const { stdout: output } = await promisify(execFile)(
    process.execPath,
    [
      "--input-type=module",
      "-e",
      "import {pathToFileURL} from 'node:url'; const {default:p}=await import(pathToFileURL(process.argv[1])); const tools=[]; const close=await p.setup({tool:{transform:async edit=>{edit({add:t=>tools.push(t)});return {dispose:async()=>{}}}}}); const reply=await tools.find(t=>t.name==='scheduler_list').execute({}, {signal:new AbortController().signal}); if(!JSON.parse(reply.content[0].text).ok)throw new Error('RPC failed'); await close(); console.log(p.id)",
      join(outside, "package/dist/index.js"),
    ],
    { cwd: outside, encoding: "utf8", windowsHide: true, timeout: 15000 },
  );
  assert.equal(output.trim(), "opencode-scheduler");
  assert.equal(f.requests.at(-1)?.body.op, "schedule_list");
  for (const { path } of metadata.files) {
    const body = await readFile(join(outside, "package", path), "utf8");
    assert.ok(!body.includes(pluginRoot));
    assert.ok(!body.includes(f.token));
    assert.ok(!/sourceMappingURL|[A-Za-z]:\\(?:Users|Dev)\\/.test(body));
  }
});
