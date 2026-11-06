import assert from "node:assert/strict";
import { test } from "node:test";
import type { Plugin } from "@opencode/plugin";
import type { Info, ToolContext } from "@opencode/plugin/promise/tool";
import plugin, { setup } from "../src/index.js";
import { SchedulerClient } from "../src/client.js";
import { created, fixture, input, page } from "./fixture.js";

test("official plugin registers six effective tools with strict protocol DTO schemas", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  const tools: Info[] = [];
  let disposed = false;
  const ctx = {
    tool: {
      transform: async (edit: (editor: any) => void) => {
        edit({ add: (tool: Info) => tools.push(tool) });
        return {
          dispose: async () => {
            disposed = true;
          },
        };
      },
    },
  } as unknown as Plugin.Context;
  const cleanup = await setup(ctx, new SchedulerClient(f.configPath));
  assert.equal(plugin.id, "opencode-scheduler");
  assert.deepEqual(
    tools.map((tool) => tool.name),
    [
      "scheduler_create",
      "scheduler_list",
      "scheduler_show",
      "scheduler_cancel",
      "scheduler_log",
      "scheduler_status",
    ],
  );
  for (const tool of tools)
    assert.equal((tool.input as any).additionalProperties, false);
  const schema = tools[0].input as any;
  assert.ok(schema.required.includes("request_id"));
  assert.equal(
    schema.properties.request_id.pattern,
    "^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-4[0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}$",
  );
  assert.deepEqual(schema.properties.target.properties.kind.enum, [
    "id",
    "title",
  ]);
  assert.deepEqual(schema.properties.delivery.enum, ["steer", "queue"]);
  assert.equal((tools[1].input as any).properties.limit.maximum, 100);

  const signal = new AbortController().signal;
  const context = { signal } as ToolContext;
  for (const title of [false, true]) {
    f.setRpc(async (_body, res) => {
      res.end(JSON.stringify(created(title)));
    });
    const result = await tools[0].execute(
      {
        ...input,
        target: title ? { kind: "title", value: "Shared title" } : input.target,
      },
      context,
    );
    assert.deepEqual(result.content, [
      { type: "text", text: JSON.stringify(created(title)) },
    ]);
  }
  f.setRpc(async (body, res) => {
    res.end(
      JSON.stringify(
        body.op === "schedule_list"
          ? page
          : {
              ok: true,
              result: {
                kind: (
                  {
                    schedule_show: "schedule",
                    schedule_cancel: "cancel",
                    schedule_log: "log",
                    status: "status",
                  } as any
                )[body.op],
                data: {},
              },
            },
      ),
    );
  });
  for (const tool of tools.slice(1))
    await tool.execute(
      tool.name === "scheduler_status" || tool.name === "scheduler_list"
        ? {}
        : { id: input.request_id },
      context,
    );
  assert.deepEqual(
    f.requests.filter((r) => r.body).map((r) => r.body.op),
    [
      "schedule_create",
      "schedule_create",
      "schedule_list",
      "schedule_show",
      "schedule_cancel",
      "schedule_log",
      "status",
    ],
  );
  await cleanup();
  assert.equal(disposed, true);
  assert.ok(!f.requests.some((r) => r.body?.op === "daemon_stop"));
});

test("tool cancellation propagates, rather than rendering an apparent success", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  const tools: Info[] = [];
  const ctx = {
    tool: {
      transform: async (edit: (editor: any) => void) => {
        edit({ add: (tool: Info) => tools.push(tool) });
        return { dispose: async () => {} };
      },
    },
  } as unknown as Plugin.Context;
  const cleanup = await setup(ctx, new SchedulerClient(f.configPath));
  t.after(cleanup);
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(
    tools[0].execute(input, { signal: controller.signal } as ToolContext),
    { name: "AbortError" },
  );
  assert.equal(f.requests.length, 0);
});
