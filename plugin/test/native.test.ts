import assert from "node:assert/strict";
import { test } from "node:test";
import childProcess, { execFile } from "node:child_process";
import { syncBuiltinESMExports } from "node:module";
import { access, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { SchedulerClient } from "../src/client.js";
import { fixture, page } from "./fixture.js";

test("native helper budget includes valid combined preparation, startup and post-start status", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  await rm(join(f.home, "endpoint.json"));
  const original = childProcess.execFile;
  // Substitute only the external executable. Real execFile enforces the production timeout.
  // Valid CLI stages: 14s preparation (<15), 27s startup (<30), 10s status (<35).
  t.mock.method(
    childProcess,
    "execFile",
    (binary: string, args: string[], options: any, callback: any) => {
      assert.equal(binary, join(f.home, "missing-binary"));
      assert.deepEqual(args, ["daemon", "start"]);
      const script = `import {writeFileSync} from 'node:fs';
      setTimeout(() => setTimeout(() => {
        writeFileSync(process.argv[1], process.argv[2]);
        setTimeout(() => process.exit(0), 10000);
      }, 27000), 14000);`;
      return original(
        process.execPath,
        [
          "--input-type=module",
          "-e",
          script,
          join(f.home, "endpoint.json"),
          JSON.stringify(f.endpoint),
        ],
        options,
        callback,
      );
    },
  );
  syncBuiltinESMExports();
  t.after(() => {
    t.mock.restoreAll();
    syncBuiltinESMExports();
  });
  const client = new SchedulerClient(f.configPath);
  t.after(() => client.dispose());
  assert.deepEqual(await client.call("schedule_list", {}), page);
  assert.deepEqual(
    f.requests.map((r) => r.path),
    ["/v1/health", "/v1/rpc"],
  );
});

test("failed start/status helper gives truthful status recovery without leaking stderr", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  await rm(join(f.home, "endpoint.json"));
  const original = childProcess.execFile;
  t.mock.method(
    childProcess,
    "execFile",
    (_binary: string, _args: string[], options: any, callback: any) =>
      original(
        process.execPath,
        ["-e", "console.error('synthetic private stderr'); process.exit(1)"],
        options,
        callback,
      ),
  );
  syncBuiltinESMExports();
  t.after(() => {
    t.mock.restoreAll();
    syncBuiltinESMExports();
  });
  const client = new SchedulerClient(f.configPath);
  t.after(() => client.dispose());
  await assert.rejects(
    client.call("schedule_list", {}),
    (error: Error) =>
      error.message.includes("opencode-scheduler status") &&
      error.message.includes("may already be running") &&
      !error.message.includes("synthetic private stderr"),
  );
});

test("configured native daemon start uses isolated data home; status and unload never start/stop it", async (t) => {
  const binary = fileURLToPath(
    new URL(
      `../../target/debug/opencode-scheduler${process.platform === "win32" ? ".exe" : ""}`,
      import.meta.url,
    ),
  );
  if (
    !(await access(binary).then(
      () => true,
      () => false,
    ))
  ) {
    t.skip(
      "Build the native binary with cargo build for this integration fixture.",
    );
    return;
  }
  const f = await fixture();
  const launches: any[][] = [];
  const originalExecFile = childProcess.execFile;
  t.mock.method(childProcess, "execFile", (...args: any[]) => {
    launches.push(args);
    return Reflect.apply(originalExecFile, childProcess, args);
  });
  syncBuiltinESMExports();
  const home = join(f.home, "native-home");
  await writeFile(
    f.configPath,
    JSON.stringify({ protocol: 1, binary, data_home: home }),
  );
  const client = new SchedulerClient(f.configPath);
  t.after(async () => {
    client.dispose();
    try {
      await promisify(execFile)(binary, ["--json", "daemon", "stop"], {
        env: { ...process.env, OPENCODE_SCHEDULER_HOME: home },
        windowsHide: true,
        timeout: 20000,
        maxBuffer: 65536,
      });
    } finally {
      t.mock.restoreAll();
      syncBuiltinESMExports();
      await f.close();
    }
  });
  const before = await client.status();
  assert.equal(before.ok, true);
  assert.equal(before.result.data.daemon.reachable, false);
  await assert.rejects(access(home));
  assert.equal(launches.length, 0);
  // Child-only discovery isolation: no real sessions, admission, or service bootstrap is involved.
  const oldStateHome = process.env.XDG_STATE_HOME;
  process.env.XDG_STATE_HOME = join(f.home, "no-opencode-state");
  try {
    assert.deepEqual(await client.call("schedule_list", {}), page);
  } finally {
    if (oldStateHome === undefined) delete process.env.XDG_STATE_HOME;
    else process.env.XDG_STATE_HOME = oldStateHome;
  }
  const endpoint = JSON.parse(
    await readFile(join(home, "endpoint.json"), "utf8"),
  );
  assert.ok(endpoint.pid > 0);
  assert.equal(launches.length, 1);
  assert.equal(launches[0][0], binary);
  assert.deepEqual(launches[0][1], ["daemon", "start"]);
  const options = launches[0][2];
  assert.equal(options.env.OPENCODE_SCHEDULER_HOME, home);
  assert.equal(options.windowsHide, true);
  assert.equal(options.shell, false);
  assert.ok(options.signal instanceof AbortSignal);
  assert.ok(options.maxBuffer > 0 && options.maxBuffer <= 65536);
  client.dispose();
  const observer = new SchedulerClient(f.configPath);
  t.after(() => observer.dispose());
  const after = await observer.status();
  assert.equal(after.ok, true);
  assert.equal(after.result.data.daemon.reachable, true);
  assert.equal(launches.length, 1);
  assert.deepEqual(after.result.data.schedules, {
    pending: 0,
    blocked: 0,
    failed: 0,
    admission_unconfirmed: 0,
  });
});
