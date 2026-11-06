import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { existsSync } from "node:fs";
import {
  cp,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rename,
  rm,
  symlink,
  unlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test, type TestContext } from "node:test";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const root = fileURLToPath(new URL("../../", import.meta.url));
const exec = promisify(execFile);
async function safetyFixture(t: TestContext) {
  const outside = await mkdtemp(join(tmpdir(), "opencode/scheduler-safety-"));
  const release = join(outside, "release");
  const data = join(outside, "private");
  const config = join(outside, "config");
  const env = {
    ...process.env,
    OPENCODE_SCHEDULER_HOME: data,
    XDG_CONFIG_HOME: config,
    XDG_STATE_HOME: join(outside, "state"),
    XDG_DATA_HOME: join(outside, "share"),
    LOCALAPPDATA: join(outside, "local"),
    HOME: outside,
    USERPROFILE: outside,
  };
  const source = join(release, "bin/opencode-scheduler.exe");
  let daemonStarted = false;
  const native = (args: string[], home = data) => {
    if (args[0] === "daemon" && args[1] === "start") daemonStarted = true;
    return exec(source, args, {
      env: { ...env, OPENCODE_SCHEDULER_HOME: home },
      cwd: outside,
      windowsHide: true,
      timeout: 120000,
    });
  };
  t.after(async () => {
    // Negative layout cases can deliberately create an endpoint.json *directory* on
    // the RED implementation; that is not a daemon descriptor to send stop through.
    if (daemonStarted) await native(["daemon", "stop"]);
    await rm(outside, { recursive: true, force: true });
  });
  await cp(join(root, ".tmp/release/expanded-zip"), release, {
    recursive: true,
  });
  const install = async (home = data, configHome = config) => {
    const result = await exec(
      "powershell.exe",
      [
        "-NoProfile",
        "-NonInteractive",
        "-File",
        join(release, "install.ps1"),
        "-DataHome",
        home,
        "-ConfigHome",
        configHome,
        "-NoPath",
      ],
      {
        env: {
          ...env,
          OPENCODE_SCHEDULER_HOME: home,
          XDG_CONFIG_HOME: configHome,
        },
        cwd: outside,
        windowsHide: true,
        timeout: 240000,
      },
    );
    daemonStarted = true;
    return result;
  };
  return { outside, release, data, config, env, native, install };
}

test("Windows native junction preflight refuses before writes or fixture daemon shutdown", async (t) => {
  if (process.platform !== "win32")
    return t.skip("Windows junction installer boundary");
  const f = await safetyFixture(t);
  await f.native(["install", "--no-path"]);
  await f.native(["daemon", "start"]);
  for (const relative of ["app", "app/bin", "install.json"]) {
    await t.test(relative, async () => {
      // A linked native directory must not redirect candidate/backup writes or daemon restart.
      const link = join(f.data, relative);
      const parked = join(f.data, "parked-native");
      const outside = join(
        f.outside,
        "outside-" + relative.replaceAll("/", "-"),
      );
      const bin = relative === "app" ? join(outside, "bin") : outside;
      await mkdir(bin, { recursive: true });
      const sentinel = join(outside, "sentinel.txt");
      const exe = join(bin, "opencode-scheduler.exe");
      await writeFile(sentinel, "outside sentinel must survive");
      const exeBytes = Buffer.concat([
        await readFile(join(f.release, "bin/opencode-scheduler.exe")),
        Buffer.from("outside executable"),
      ]);
      await writeFile(exe, exeBytes);
      const before = JSON.parse(
        await readFile(join(f.data, "endpoint.json"), "utf8"),
      );
      const names = await readdir(f.data);
      await rename(link, parked);
      await symlink(outside, link, "junction");
      try {
        await assert.rejects(f.install(), (e: any) =>
          /contains a link/i.test(e.stdout + e.stderr),
        );
        await assert.rejects(
          f.native(["install", "--no-path", "--json"]),
          (e: any) =>
            JSON.parse(e.stdout).error.code === "private_directory_failure",
        );
        assert.equal(
          existsSync(f.config),
          false,
          "No config creation before linked-native rejection",
        );
        assert.deepEqual(await readFile(exe), exeBytes);
        assert.equal(
          await readFile(sentinel, "utf8"),
          "outside sentinel must survive",
        );
        assert.equal(
          JSON.parse(await readFile(join(f.data, "endpoint.json"), "utf8"))
            .instance_id,
          before.instance_id,
        );
        assert.equal(
          JSON.parse((await f.native(["status", "--json"])).stdout).result.data
            .daemon.reachable,
          true,
        );
      } finally {
        await unlink(link);
        await rename(parked, link);
      }
      assert.deepEqual(
        await readdir(f.data),
        names,
        "No installation control files were created",
      );
    });
  }
  await t.test("linked data-root ancestor", async () => {
    const alias = join(f.outside, "linked-parent");
    const before = await readFile(join(f.data, "endpoint.json"));
    const metadata = await readFile(join(f.data, "install.json"));
    await symlink(f.outside, alias, "junction");
    try {
      await assert.rejects(
        f.native(["install", "--no-path", "--json"], join(alias, "private")),
        (e: any) =>
          JSON.parse(e.stdout).error.code === "private_directory_failure",
      );
      assert.deepEqual(await readFile(join(f.data, "endpoint.json")), before);
      assert.deepEqual(await readFile(join(f.data, "install.json")), metadata);
      assert.equal(
        JSON.parse((await f.native(["status", "--json"])).stdout).result.data
          .daemon.reachable,
        true,
      );
    } finally {
      await unlink(alias);
    }
  });
});

test("Windows preflight covers native control files and dangling native junctions", async (t) => {
  if (process.platform !== "win32")
    return t.skip("Windows reparse-point installer boundary");
  const f = await safetyFixture(t);
  await f.native(["install", "--no-path"]);
  // These are the fixed native installer/restart paths, including SQLite and log companions.
  for (const relative of [
    "install.lock",
    "install.json",
    "startup.lock",
    "daemon.lock",
    "endpoint.json",
    "diagnostic.log",
    "diagnostic.log.1",
    "schedules.sqlite3",
    "schedules.sqlite3-wal",
    "schedules.sqlite3-shm",
    "schedules.sqlite3-journal",
    "product-install.json",
    "app/bin/opencode-scheduler.exe",
    "app/bin/opencode-scheduler.previous.exe",
    "app/bin/dangling",
  ]) {
    await t.test(relative, async () => {
      const path = join(
        f.data,
        relative === "app/bin/dangling" ? "app/bin" : relative,
      );
      const parked = join(f.data, "parked-control");
      const outside = join(f.outside, "outside-control");
      await mkdir(outside, { recursive: true });
      await writeFile(
        join(outside, "sentinel.txt"),
        "outside control sentinel",
      );
      const existed = existsSync(path);
      if (existed) await rename(path, parked);
      await symlink(
        relative === "app/bin/dangling"
          ? join(f.outside, "missing-target")
          : outside,
        path,
        "junction",
      );
      try {
        await assert.rejects(f.install(), (e: any) =>
          /contains a link/i.test(e.stdout + e.stderr),
        );
        const names = await readdir(f.data);
        await assert.rejects(
          f.native(["install", "--no-path", "--json"]),
          (e: any) =>
            JSON.parse(e.stdout).error.code === "private_directory_failure",
        );
        assert.deepEqual(
          await readdir(f.data),
          names,
          "No native writes before link rejection",
        );
        assert.equal(
          existsSync(f.config),
          false,
          "Unsafe native controls must fail before staging/config writes",
        );
        assert.equal(
          await readFile(join(outside, "sentinel.txt"), "utf8"),
          "outside control sentinel",
        );
      } finally {
        await unlink(path);
        if (existed) await rename(parked, path);
      }
    });
  }
});

test("Windows preflight rejects data roots swallowed by plugin or skill swaps", async (t) => {
  if (process.platform !== "win32")
    return t.skip("Windows custom-root installer boundary");
  const f = await safetyFixture(t);
  for (const kind of ["plugins", "skills"]) {
    for (const nested of [false, true]) {
      await t.test(`${kind} ${nested ? "nested" : "equal"}`, async () => {
        const config = join(f.outside, `${kind}-${nested}`);
        const target = join(config, "opencode", kind, "opencode-scheduler");
        const data = nested ? join(target, "nested-data") : target;
        // Must reject before even creating the config/data directories, not by a late status failure.
        await assert.rejects(f.install(data, config), (e: any) =>
          /overlap/i.test(e.stdout + e.stderr),
        );
        assert.equal(existsSync(config), false);
        assert.equal(existsSync(data), false);
      });
    }
  }
});

test("Windows supported reverse nesting keeps private data outside the swapped subtrees", async (t) => {
  if (process.platform !== "win32")
    return t.skip("Windows custom-root installer boundary");
  for (const relative of ["config", "app/bin/config"]) {
    await t.test(relative, async (s) => {
      const f = await safetyFixture(s);
      // A private data root may safely contain config, even under app/bin: only
      // native file-path collisions and data roots swallowed by swaps are unsafe.
      await f.native(["install", "--no-path"]);
      const config = join(f.data, relative);
      await f.install(f.data, config);
      assert.equal(
        existsSync(join(f.data, "app/bin/opencode-scheduler.exe")),
        true,
      );
      assert.equal(
        existsSync(
          join(config, "opencode/plugins/opencode-scheduler/install.json"),
        ),
        true,
      );
      assert.equal(
        existsSync(join(config, "opencode/skills/opencode-scheduler/SKILL.md")),
        true,
      );
    });
  }
});

test("Windows reverse overlaps cannot turn native file paths into config directories", async (t) => {
  if (process.platform !== "win32")
    return t.skip("Windows custom-root installer boundary");
  for (const relative of [
    "diagnostic.log.1",
    "schedules.sqlite3",
    "endpoint.json/config",
    "app/bin/opencode-scheduler.previous.exe",
  ]) {
    await t.test(relative, async (s) => {
      const f = await safetyFixture(s);
      await f.native(["install", "--no-path"]);
      const names = await readdir(f.data);
      const config = join(f.data, relative);
      await assert.rejects(f.install(f.data, config), (e: any) =>
        /overlap/i.test(e.stdout + e.stderr),
      );
      assert.equal(existsSync(config), false);
      assert.deepEqual(await readdir(f.data), names);
    });
  }
});
