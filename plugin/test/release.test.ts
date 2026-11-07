import assert from "node:assert/strict";
import { execFile, execFileSync, spawnSync } from "node:child_process";
import {
  mkdir,
  mkdtemp,
  readFile,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { promisify } from "node:util";
import { created, fixture, input, page, requestID } from "./fixture.js";

const root = fileURLToPath(new URL("../../", import.meta.url));
function nativeFixture() {
  const bytes = Buffer.alloc(512);
  bytes.write("MZ");
  bytes.writeUInt32LE(128, 60);
  bytes.write("PE\0\0", 128);
  bytes.writeUInt16LE(0x8664, 132);
  bytes.writeUInt16LE(240, 148);
  bytes.writeUInt16LE(0x20b, 152);
  return bytes;
}
test("release scanner enforces exact layout, native strings and runtime privacy", async (t) => {
  const home = await mkdtemp(join(root, ".tmp/scanner-"));
  t.after(() => rm(home, { recursive: true, force: true }));
  const files = [
    "bin/opencode-scheduler.exe",
    "plugin/dist/index.js",
    "plugin/package.json",
    "plugin/LICENSE",
    "plugin/THIRD-PARTY-NOTICES.txt",
    "skills/opencode-scheduler/SKILL.md",
    "README.md",
    "LICENSE",
    "THIRD-PARTY-NOTICES.txt",
  ];
  for (const file of files) {
    await mkdir(join(home, file, ".."), { recursive: true });
    await writeFile(
      join(home, file),
      file.endsWith(".exe")
        ? nativeFixture()
        : file.endsWith("package.json")
          ? '{"type":"module","exports":"./dist/index.js"}'
          : "safe",
    );
  }
  const scan = () =>
    spawnSync(
      process.execPath,
      [join(root, "scripts/check-package.mjs"), home],
      { encoding: "utf8" },
    );
  const initialScan = scan();
  assert.equal(initialScan.status, 0, initialScan.stderr);
  const debug = nativeFixture();
  debug.writeUInt32LE(1, 152 + 112 + 6 * 8);
  await writeFile(join(home, files[0]), debug);
  assert.match(scan().stderr, /PE debug directory/);
  await writeFile(join(home, files[0]), nativeFixture());
  for (const content of [
    'import x from "unshipped-sdk";',
    'const x = import("unshipped-sdk");',
    'const x = require("unshipped-sdk");',
    'export { x } from "unshipped-sdk";',
    "const x = import(variable);",
  ]) {
    await writeFile(join(home, "plugin/dist/index.js"), content);
    assert.notEqual(scan().status, 0, content);
  }
  await writeFile(join(home, "plugin/dist/index.js"), "safe");
  for (const field of [
    "dependencies",
    "optionalDependencies",
    "peerDependencies",
    "bundledDependencies",
  ]) {
    await writeFile(
      join(home, "plugin/package.json"),
      JSON.stringify({
        [field]: field === "bundledDependencies" ? ["sdk"] : { sdk: "*" },
      }),
    );
    assert.notEqual(scan().status, 0, field);
  }
  await writeFile(join(home, "plugin/package.json"), "{}");
  for (const name of [
    "debug.pdb",
    "plugin/dist/index.js.map",
    "plugin/install.json",
    "product-install.json",
    "plugin/node_modules/sdk.js",
  ]) {
    await mkdir(join(home, name, ".."), { recursive: true });
    await writeFile(join(home, name), "safe");
    assert.notEqual(scan().status, 0, name);
    await rm(join(home, name));
  }
  await rm(join(home, "plugin/node_modules"), { recursive: true });
  const restoredLayoutScan = scan();
  assert.equal(restoredLayoutScan.status, 0, restoredLayoutScan.stderr);
  await mkdir(join(home, "unexpected-empty"));
  assert.notEqual(scan().status, 0);
  await rm(join(home, "unexpected-empty"), { recursive: true });
  await symlink(join(home, "plugin"), join(home, "linked"), "junction");
  assert.match(scan().stderr, /Link forbidden/);
  await rm(join(home, "linked"), { recursive: true });
  const removedLinkScan = scan();
  assert.equal(removedLinkScan.status, 0, removedLinkScan.stderr);
  for (const encoding of ["utf8", "utf16le", "utf16be"] as const) {
    for (const privateText of [
      root,
      process.env.USERPROFILE || process.env.HOME!,
      "Bearer " + "a".repeat(48),
      "RSDS",
      "sourceMappingURL=data:",
    ]) {
      const bytes = Buffer.from(
        privateText,
        encoding === "utf16be" ? "utf16le" : encoding,
      );
      if (encoding === "utf16be") bytes.swap16();
      await writeFile(
        join(home, files[0]),
        Buffer.concat([nativeFixture(), bytes]),
      );
      assert.notEqual(scan().status, 0, privateText);
    }
  }
  await writeFile(join(home, files[0]), nativeFixture());
  const restoredNativeScan = scan();
  assert.equal(restoredNativeScan.status, 0, restoredNativeScan.stderr);
  await rm(join(home, "LICENSE"));
  assert.match(scan().stderr, /Missing files: LICENSE/);
});

test("bundle notices include only actual SDK helper contributions and reject unreviewed code", async () => {
  const { nodeNotices } = await import(
    new URL("../../scripts/notices.mjs", import.meta.url).href
  );
  const metadata = JSON.parse(
    await readFile(join(root, ".tmp/release/plugin-meta.json"), "utf8"),
  );
  const notice = await nodeNotices(root, metadata);
  assert.match(notice, /@opencode\/plugin 2\.2\.0 \(MIT\)/);
  assert.match(notice, /Copyright \(c\) 2025 opencode/);
  assert.doesNotMatch(notice, /minimatch|glob|esbuild/);
  await assert.rejects(
    nodeNotices(root, {
      outputs: {
        bundle: {
          inputs: {
            "plugin/node_modules/glob/dist/index.js": { bytesInOutput: 1 },
          },
        },
      },
    }),
    /Unreviewed bundled contribution/,
  );
});

test("Rust source package contains only reviewed source inputs, never runtime config", () => {
  const files = execFileSync(
    "cargo",
    ["package", "--locked", "--list", "--allow-dirty"],
    { cwd: root, encoding: "utf8", windowsHide: true },
  )
    .trim()
    .split(/\r?\n/);
  for (const file of files)
    assert.match(
      file.replaceAll("\\", "/"),
      /^(?:src\/[^/]+\.rs|tests\/[^/]+\.rs|\.cargo\/config.toml|Cargo\.(?:toml(?:\.orig)?|lock)|README.md|LICENSE|\.cargo_vcs_info.json)$/,
    );
});

test("explicit-only skill contains retrievable, real CLI command examples", async (t) => {
  const text = await readFile(
    join(root, "skills/opencode-scheduler/SKILL.md"),
    "utf8",
  );
  assert.match(
    text,
    /^---\r?\nname: opencode-scheduler\r?\ndescription: Use when explicitly/,
  );
  assert.match(text, /metadata:\r?\n  opencode\/autoinvoke: false\r?\n---/);
  for (const phrase of [
    /ALL/,
    /literal/i,
    /six/i,
    /10\/20\/40\/80\/160/,
    /request-id/,
    /identical/,
    /uncertain/i,
    /admitted/,
    /secret/i,
    /database/i,
    /timezone/i,
    /proactive/i,
    /5:00/,
    /next future local/i,
  ])
    assert.match(text, phrase);
  const binary = join(
    root,
    "target/debug/opencode-scheduler" +
      (process.platform === "win32" ? ".exe" : ""),
  );
  const examples = [...text.matchAll(/^opencode-scheduler (.+)$/gm)].map(
    (m) => m[1],
  );
  assert.ok(examples.length >= 6);
  for (const example of examples) {
    const words = example.match(/'[^']*'|\S+/g)!;
    const command =
      words[0] === "schedule" ? words.slice(0, 2) : words.slice(0, 1);
    const help = execFileSync(binary, [...command, "--help"], {
      encoding: "utf8",
      windowsHide: true,
    });
    for (const flag of words.filter((w) => w.startsWith("--")))
      assert.ok(help.includes(flag), flag);
  }
  const f = await fixture();
  t.after(() => f.close());
  f.setRpc(async (body, response) => {
    response.end(
      JSON.stringify(body.op === "schedule_create" ? created() : page),
    );
  });
  // mkdtemp inherits parent ACLs; let native preparation create a private child.
  const privateHome = join(f.home, "private-native-home");
  const run = (args: string[]) =>
    promisify(execFile)(binary, args, {
      env: { ...process.env, OPENCODE_SCHEDULER_HOME: privateHome },
      windowsHide: true,
      timeout: 30000,
    });
  await run(["install", "--no-path"]);
  await writeFile(
    join(privateHome, "endpoint.json"),
    JSON.stringify(f.endpoint),
  );
  const create = examples
    .find((e) => e.startsWith("schedule create"))!
    .match(/'[^']*'|\S+/g)!
    .map((w) => w.replace(/^'|'$/g, ""));
  create[create.indexOf("--prompt") + 1] = input.prompt;
  create.push("--request-id", requestID);
  await run(create);
  await run(create);
  assert.equal(
    f.requests.filter((r) => r.body?.op === "schedule_create").length,
    2,
  );
  for (const request of f.requests.filter(
    (r) => r.body?.op === "schedule_create",
  )) {
    assert.equal(request.body.input.prompt, input.prompt);
    assert.equal(request.body.input.request_id, requestID);
    assert.equal(request.body.input.target.value, "ses_example");
    assert.equal(request.body.input.at, "2099-11-06T14:00:00+01:00");
  }
  const titleExample = examples.find((e) => e.includes("--session-title"));
  assert.ok(titleExample, "Skill must provide an exact-title clock example");
  const titleCreate = titleExample
    .match(/'[^']*'|\S+/g)!
    .map((w) => w.replace(/^'|'$/g, ""));
  f.setRpc(async (_body, response) => {
    response.end(JSON.stringify(created(true)));
  });
  await run(titleCreate);
  assert.deepEqual(f.requests.at(-1)?.body.input.target, {
    kind: "title",
    value: "Exact title",
  });
  assert.equal(f.requests.at(-1)?.body.input.at, "5:00");
  f.setRpc(async (_body, response) => {
    response.end(JSON.stringify(page));
  });
  assert.match((await run(["schedule", "list"])).stdout, /schedule/i);
  const schedule = {
    ...created().result.data.schedules[0],
    prompt: input.prompt,
  };
  f.setRpc(async (_body, response) => {
    response.end(
      JSON.stringify({
        ok: true,
        result: { kind: "schedule", data: schedule },
      }),
    );
  });
  const humanShow = await run(["schedule", "show", schedule.id]);
  assert.ok(humanShow.stdout.includes(schedule.id));
  assert.doesNotMatch(humanShow.stdout, /literal|\$\{text\}|@file|\/command/);
  const jsonShow = await run(["schedule", "show", schedule.id, "--json"]);
  assert.equal(JSON.parse(jsonShow.stdout).result.data.prompt, input.prompt);
  assert.match(text, /Plain `list` and `show` omit prompt text/);
  assert.match(text, /`opencode-scheduler schedule show <schedule-id> --json`/);
});
