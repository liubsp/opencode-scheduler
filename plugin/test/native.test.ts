import assert from "node:assert/strict";
import { test } from "node:test";
import childProcess, { execFile } from "node:child_process";
import { syncBuiltinESMExports } from "node:module";
import { access, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { SchedulerClient } from "../src/client.js";
import { fixture, page } from "./fixture.js";

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
  assert.ok(options.timeout > 0 && options.timeout <= 20000);
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
