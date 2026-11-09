import * as Plugin from "@opencode/plugin/promise/plugin";
import { SchedulerClient, uuidPattern } from "./client.js";

const id = { type: "string", pattern: uuidPattern };
const paging = {
  limit: { type: "integer", minimum: 1, maximum: 100, default: 50 },
  cursor: { type: "string", minLength: 1, maxLength: 512 },
};
const definitions = [
  {
    name: "create",
    op: "schedule_create",
    description:
      "Schedule a literal prompt once. ID selects one session; exact title freezes ALL matches. Retain request_id: uncertain/aborted calls may be accepted; retry only identical input with the same UUID. Abort/unload does not cancel schedules. Time: RFC 3339 with offset, local YYYY-MM-DD HH:MM[:SS], or H:MM[:SS] (next future local occurrence). Local DST gaps/overlaps are rejected. Admission is not agent completion.",
    properties: {
      request_id: {
        ...id,
        description:
          "UUID v4; reuse with identical input after a lost response.",
      },
      target: {
        type: "object",
        additionalProperties: false,
        properties: {
          kind: { type: "string", enum: ["id", "title"] },
          value: { type: "string", minLength: 1, maxLength: 1024 },
        },
        required: ["kind", "value"],
      },
      at: { type: "string", minLength: 1, maxLength: 128 },
      prompt: {
        type: "string",
        minLength: 1,
        maxLength: 65536,
        description:
          "Literal prompt; no interpolation, file expansion or command execution.",
      },
      delivery: { type: "string", enum: ["steer", "queue"], default: "steer" },
    },
    required: ["request_id", "target", "at", "prompt"],
  },
  {
    name: "list",
    op: "schedule_list",
    description:
      "List scheduled prompts with cursor pagination (prompt text omitted).",
    properties: paging,
    required: [],
  },
  {
    name: "show",
    op: "schedule_show",
    description:
      "Show a schedule including its literal prompt and admission certainty.",
    properties: { id },
    required: ["id"],
  },
  {
    name: "cancel",
    op: "schedule_cancel",
    description:
      "Cancel pending delivery; cannot retract admission or guarantee cancellation if uncertain.",
    properties: { id },
    required: ["id"],
  },
  {
    name: "log",
    op: "schedule_log",
    description:
      "Read paginated delivery attempts and admission certainty for one schedule.",
    properties: { id, ...paging },
    required: ["id"],
  },
  {
    name: "status",
    op: "status",
    description: "Read scheduler status without starting either service.",
    properties: {},
    required: [],
  },
];

export async function setup(
  ctx: Pick<Plugin.Context, "tool">,
  client = new SchedulerClient(),
) {
  let registration;
  try {
    registration = await ctx.tool.transform((editor) => {
      for (const definition of definitions) {
        editor.add({
          name: `scheduler_${definition.name}`,
          description: definition.description,
          input: {
            type: "object",
            properties: definition.properties,
            required: definition.required,
            additionalProperties: false,
          },
          execute: async (input, context) => ({
            content: [
              {
                type: "text",
                text: JSON.stringify(
                  await client.call(definition.op, input, context.signal),
                ),
              },
            ],
          }),
        });
      }
    });
  } catch (error) {
    client.dispose();
    throw error;
  }
  return async () => {
    client.dispose();
    await registration.dispose();
  };
}

export default Plugin.define({ id: "opencode-scheduler", setup });
