import assert from "node:assert/strict";
import { test } from "node:test";
import { readdir, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { execFile } from "node:child_process";
import { createServer } from "node:http";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";
import type { Socket } from "node:net";
import { SchedulerClient } from "../src/client.js";
import { created, fixture, input, page, requestID } from "./fixture.js";

async function bounded<T>(pending: Promise<T>, message: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      pending,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(() => reject(new Error(message)), 1000);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

test("authenticated health precedes unchanged protocol-1 RPC, without Origin", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  const client = new SchedulerClient(f.configPath);
  t.after(() => client.dispose());
  assert.deepEqual(await client.call("schedule_list", { limit: 10 }), page);
  assert.deepEqual(
    f.requests.map((r) => r.path),
    ["/v1/health", "/v1/rpc"],
  );
  assert.deepEqual(f.requests[1].body, {
    op: "schedule_list",
    input: { limit: 10 },
  });
  assert.equal(f.requests[1].headers.origin, undefined);
});

test("host environment proxy settings cannot receive scheduler credentials", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  let proxyRequests = 0;
  const proxy = createServer((_request, response) => {
    proxyRequests++;
    response.writeHead(502).end("{}");
  });
  await new Promise<void>((resolve) => proxy.listen(0, "127.0.0.1", resolve));
  t.after(async () => {
    proxy.closeAllConnections();
    await new Promise<void>((resolve) => proxy.close(() => resolve()));
  });
  const address = proxy.address();
  assert.ok(address && typeof address !== "string");
  const proxyURL = `http://127.0.0.1:${address.port}`;
  const script =
    "import {pathToFileURL} from 'node:url'; const {SchedulerClient}=await import(pathToFileURL(process.argv[1])); const c=new SchedulerClient(process.argv[2]); console.log(JSON.stringify(await c.call('schedule_list',{}))); c.dispose()";
  const { stdout } = await promisify(execFile)(
    process.execPath,
    [
      "--input-type=module",
      "-e",
      script,
      fileURLToPath(new URL("../dist/client.js", import.meta.url)),
      f.configPath,
    ],
    {
      windowsHide: true,
      timeout: 10000,
      maxBuffer: 65536,
      env: {
        ...process.env,
        NODE_USE_ENV_PROXY: "1",
        HTTP_PROXY: proxyURL,
        HTTPS_PROXY: proxyURL,
        ALL_PROXY: proxyURL,
        NO_PROXY: "",
        http_proxy: proxyURL,
        https_proxy: proxyURL,
        no_proxy: "",
      },
    },
  );
  assert.deepEqual(JSON.parse(stdout), page);
  assert.equal(proxyRequests, 0);
});

test("ID and exact-title batches retain frozen results and explicit retry identity", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  const client = new SchedulerClient(f.configPath);
  t.after(() => client.dispose());
  for (const title of [false, true]) {
    f.setRpc(async (_body, res) => {
      res.end(JSON.stringify(created(title)));
    });
    const request = {
      ...input,
      target: title ? { kind: "title", value: "Shared title" } : input.target,
    };
    assert.deepEqual(
      await client.call("schedule_create", request),
      created(title),
    );
    assert.deepEqual(
      await client.call("schedule_create", request),
      created(title),
    );
    const calls = f.requests.filter((r) => r.path === "/v1/rpc").slice(-2);
    assert.deepEqual(calls[0].body, { op: "schedule_create", input: request });
    assert.deepEqual(calls[1].body, calls[0].body);
  }
});

test("request is serialized before asynchronous discovery", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  const client = new SchedulerClient(f.configPath);
  t.after(() => client.dispose());
  f.setRpc(async (_body, res) => {
    res.end(JSON.stringify(created()));
  });
  const request = structuredClone(input);
  const pending = client.call("schedule_create", request);
  request.prompt = "changed after invocation";
  await pending;
  assert.equal(f.requests.at(-1)!.body.input.prompt, input.prompt);
});

test("abort after durable admission stops the wait, never cancels or retries accepted work", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  const client = new SchedulerClient(f.configPath);
  t.after(() => client.dispose());
  const controller = new AbortController();
  const accepted = new Map<string, unknown>();
  f.setRpc(async (body, res) => {
    accepted.set(body.input.request_id, created());
    controller.abort();
    res.end(JSON.stringify(created()));
  });
  await assert.rejects(
    client.call("schedule_create", input, controller.signal),
    { name: "AbortError" },
  );
  assert.deepEqual(accepted.get(requestID), created());
  assert.deepEqual(
    f.requests.filter((r) => r.body).map((r) => r.body.op),
    ["schedule_create"],
  );
  f.setRpc(async (body, res) => {
    res.end(JSON.stringify(accepted.get(body.input.request_id)));
  });
  assert.deepEqual(await client.call("schedule_create", input), created());
});

test("dispose aborts only owned waits, leaves daemon usable and sends no stop", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  const client = new SchedulerClient(f.configPath);
  f.setRpc(async (_body, _res) => {
    client.dispose();
  });
  await assert.rejects(client.call("schedule_list", {}), {
    name: "AbortError",
  });
  const next = new SchedulerClient(f.configPath);
  t.after(() => next.dispose());
  f.setRpc(async (_body, res) => {
    res.end(JSON.stringify(page));
  });
  assert.deepEqual(await next.call("schedule_list", {}), page);
  assert.ok(!f.requests.some((r) => r.body?.op === "daemon_stop"));
});

test("status and call(status) do not start a missing daemon or create runtime files", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  await rm(join(f.home, "endpoint.json"));
  const before = await readdir(f.home);
  const client = new SchedulerClient(f.configPath);
  t.after(() => client.dispose());
  for (const result of [
    await client.status(),
    await client.call("status", {}),
  ]) {
    assert.equal(result.ok, true);
    assert.equal(result.result.kind, "status");
    assert.equal(result.result.data.daemon.reachable, false);
    assert.equal(result.result.data.schedules.pending, null);
  }
  assert.equal(f.requests.length, 0);
  assert.deepEqual(await readdir(f.home), before);
  await assert.rejects(client.call("daemon_stop", {}), /operation/i);
});

test("domain errors keep code and safe canonical wording, never credentials", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  const client = new SchedulerClient(f.configPath);
  t.after(() => client.dispose());
  for (const [code, message] of [
    ["already_admitted", "Prompt is already admitted."],
    ["outcome_uncertain", "Prompt admission is unconfirmed."],
    ["request_conflict", "Request ID has different original input."],
  ]) {
    f.setRpc(async (_body, res) => {
      res
        .writeHead(409)
        .end(JSON.stringify({ ok: false, error: { code, message: f.token } }));
    });
    assert.deepEqual(await client.call("schedule_cancel", { id: requestID }), {
      ok: false,
      error: { code, message },
    });
  }
});

test("invalid endpoint and health instance/protocol reject before RPC", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  const client = new SchedulerClient(f.configPath);
  t.after(() => client.dispose());
  for (const changes of [
    { protocol: 2 },
    { instance_id: "bad" },
    { token: "bad" },
    { pid: 0 },
    { base_url: "http://localhost:1234" },
    { base_url: "http://[::1]:1234" },
    { base_url: "http://127.0.0.1:1234/other" },
    { base_url: "http://user@127.0.0.1:1234" },
    { base_url: "http://127.0.0.1:1234?x=1" },
    { base_url: "https://127.0.0.1:1234" },
  ]) {
    await writeFile(
      join(f.home, "endpoint.json"),
      JSON.stringify({ ...f.endpoint, ...changes }),
    );
    await assert.rejects(client.call("schedule_list", {}));
  }
  assert.equal(f.requests.length, 0);
  await writeFile(join(f.home, "endpoint.json"), JSON.stringify(f.endpoint));
  for (const value of [
    { protocol: 2, instance_id: f.endpoint.instance_id, binary_version: "x" },
    { protocol: 1, instance_id: requestID, binary_version: "x" },
  ]) {
    f.setHealth(value);
    await assert.rejects(client.call("schedule_list", {}), /protocol/i);
  }
  assert.ok(f.requests.every((r) => r.path === "/v1/health"));
});

test("redirects and health authentication failures are never followed or started over", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  const client = new SchedulerClient(f.configPath);
  t.after(() => client.dispose());
  f.setHealthHandler((res) => {
    res.writeHead(302, { Location: "/v1/rpc" }).end();
  });
  await assert.rejects(client.call("schedule_list", {}), /redirect/i);
  assert.deepEqual(
    f.requests.map((r) => r.path),
    ["/v1/health"],
  );
  f.setHealthHandler((res) => {
    res.writeHead(401).end(JSON.stringify({ token: f.token }));
  });
  await assert.rejects(client.call("schedule_list", {}), /authentication/i);
  assert.equal(f.requests.length, 2);
  f.setHealthHandler();
  f.setRpc(async (_body, res) => {
    res.writeHead(307, { Location: "/v1/health" }).end();
  });
  await assert.rejects(client.call("schedule_list", {}), /redirect/i);
  assert.equal(f.requests.length, 4);
});

test("health deadlines bound a stalled peer, including a partial streaming response", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  const client = new SchedulerClient(f.configPath);
  t.after(() => client.dispose());
  f.setHealthHandler((res) => {
    res.writeHead(200);
    res.write("{");
  });
  const started = performance.now();
  await assert.rejects(client.call("schedule_list", {}), /timed out/i);
  assert.ok(performance.now() - started < 6000);
  assert.equal(f.requests.length, 1);
});

for (const phase of ["health", "RPC"]) {
  for (const cancellation of ["caller abort", "client dispose"]) {
    test(`HTTP 101 during ${phase} rejects and closes its socket despite later ${cancellation}`, async (t) => {
      const f = await fixture();
      const client = new SchedulerClient(f.configPath);
      const controller = new AbortController();
      let socket: Socket | undefined;
      let socketClosed!: () => void;
      const closed = new Promise<void>((resolve) => {
        socketClosed = resolve;
      });
      t.after(async () => {
        client.dispose();
        socket?.destroy();
        await f.close();
      });
      const upgrade = (response: import("node:http").ServerResponse) => {
        socket = response.socket!;
        socket.once("close", socketClosed);
        response.writeHead(101, { Connection: "Upgrade", Upgrade: "fixture" });
        response.end();
      };
      if (phase === "health") f.setHealthHandler(upgrade);
      else f.setRpc(async (_body, response) => upgrade(response));
      const outcome = client.call("schedule_list", {}, controller.signal).then(
        () => ({ error: undefined }),
        (error: Error) => ({ error }),
      );
      // Cancellation after transport close reproduced the reviewer's permanently pending wait.
      await bounded(closed, "HTTP 101 left its fixture socket open");
      if (cancellation === "caller abort") controller.abort();
      else client.dispose();
      const { error } = await bounded(
        outcome,
        "HTTP 101 left the client operation pending",
      );
      assert.ok(
        error,
        "HTTP upgrades must not be accepted as scheduler replies",
      );
      assert.match(error.message, /upgrade/i);
      assert.deepEqual(
        f.requests.map((request) => request.path),
        phase === "health" ? ["/v1/health"] : ["/v1/health", "/v1/rpc"],
      );
    });
  }
}

test("malformed/wrong-kind responses fail closed without exposing raw response data", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  const client = new SchedulerClient(f.configPath);
  t.after(() => client.dispose());
  for (const response of [
    "not-json",
    JSON.stringify({ ok: true, result: { kind: "created", data: {} } }),
    JSON.stringify({
      ok: false,
      error: { code: f.token, message: f.endpoint.base_url },
    }),
  ]) {
    f.setRpc(async (_body, res) => {
      res.end(response);
    });
    await assert.rejects(
      client.call("schedule_list", {}),
      /Invalid scheduler response/,
    );
  }
});

test("even an authenticated malformed success cannot leak the endpoint credential", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  const client = new SchedulerClient(f.configPath);
  t.after(() => client.dispose());
  f.setRpc(async (_body, res) => {
    res.end(
      JSON.stringify({
        ok: true,
        result: {
          kind: "page",
          data: { items: [{ token: f.token }], next_cursor: null },
        },
      }),
    );
  });
  await assert.rejects(
    client.call("schedule_list", {}),
    (error: Error) => !error.message.includes(f.token),
  );
});

test("malformed domain error fields are rejected, not coerced into valid protocol errors", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  const client = new SchedulerClient(f.configPath);
  t.after(() => client.dispose());
  for (const error of [
    { code: ["already_admitted"], message: "bad" },
    { code: "already_admitted", message: null },
    { code: "already_admitted", message: "" },
    { code: "already_admitted", message: "x".repeat(513) },
  ]) {
    f.setRpc(async (_body, res) => {
      res.writeHead(409).end(JSON.stringify({ ok: false, error }));
    });
    await assert.rejects(
      client.call("schedule_cancel", { id: requestID }),
      /Invalid scheduler response/,
    );
  }
});

test("creation failure reports the original frozen identity even if caller mutates input", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  const client = new SchedulerClient(f.configPath);
  t.after(() => client.dispose());
  const request = structuredClone(input);
  f.setRpc(async (_body, res) => {
    res.destroy();
  });
  const pending = client.call("schedule_create", request);
  request.request_id = "mutated-private-text";
  await assert.rejects(
    pending,
    (error: Error) =>
      error.message.includes(requestID) &&
      !error.message.includes("mutated-private-text"),
  );
});

test("oversized descriptors, request bodies and streamed responses are bounded", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  const client = new SchedulerClient(f.configPath);
  t.after(() => client.dispose());
  await writeFile(join(f.home, "endpoint.json"), " ".repeat(65537));
  await assert.rejects(client.call("schedule_list", {}), /limit/i);
  await writeFile(join(f.home, "endpoint.json"), JSON.stringify(f.endpoint));
  await assert.rejects(
    client.call("schedule_create", { ...input, prompt: "x".repeat(262145) }),
    /limit/i,
  );
  f.setRpc(async (_body, res) => {
    res.end(" ".repeat(16 * 1024 * 1024 + 1));
  });
  await assert.rejects(client.call("schedule_list", {}), /limit/i);
});

test("invalid installation fails closed without executing paths or leaking them", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  const client = new SchedulerClient(f.configPath);
  t.after(() => client.dispose());
  for (const config of [
    { protocol: 2, binary: process.execPath, data_home: f.home },
    { protocol: 1, binary: "node", data_home: f.home },
    { protocol: 1, binary: process.execPath, data_home: "relative" },
    {
      protocol: 1,
      binary: process.execPath,
      data_home: f.home,
      token: f.token,
    },
  ]) {
    await writeFile(f.configPath, JSON.stringify(config));
    await assert.rejects(
      client.call("schedule_list", {}),
      (error: Error) =>
        !error.message.includes(f.home) && !error.message.includes(f.token),
    );
  }
  assert.equal(f.requests.length, 0);
});
