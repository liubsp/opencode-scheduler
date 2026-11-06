import {
  createServer,
  type IncomingMessage,
  type ServerResponse,
} from "node:http";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { join } from "node:path";

export const requestID = "cc0321c3-3160-44dc-8035-6b399731f702";
export const instanceID = "c10ee051-9ad1-428e-8097-2bf4a62d7628";
export const input = {
  request_id: requestID,
  target: { kind: "id", value: "ses_fixture" },
  at: "2099-11-06T14:00:00Z",
  prompt: "  literal ${text}\n@file /command  ",
  delivery: "queue",
};
export function created(title = false) {
  return {
    ok: true,
    result: {
      kind: "created",
      data: {
        request_id: requestID,
        schedules: (title ? ["ses_first", "ses_second"] : ["ses_fixture"]).map(
          (id, i) => ({
            id: `f6b3d449-7023-4f81-8b85-5c927790e20${i}`,
            request_id: requestID,
            created_at: "2026-11-06T12:00:00Z",
            time: {
              due_at: "2099-11-06T14:00:00Z",
              original: input.at,
              display: input.at,
              timezone: null,
            },
            target: { session_id: id, title: title ? "Shared title" : null },
            delivery: "queue",
            message_id: `msg_f6b3d44970234f818b855c927790e20${i}`,
            state: "pending",
            admitted_at: null,
            attempts_used: 0,
            next_attempt_at: null,
            certainty: "not_admitted",
            last_error: null,
            lateness_ms: null,
          }),
        ),
      },
    },
  };
}
export const page = {
  ok: true,
  result: { kind: "page", data: { items: [], next_cursor: null } },
};

export async function fixture() {
  const root = fileURLToPath(
    new URL("../../.tmp/plugin-tests/", import.meta.url),
  );
  await mkdir(root, { recursive: true });
  const home = await mkdtemp(join(root, "case-"));
  const configPath = join(home, "install.json");
  const token = "ab".repeat(48);
  const requests: {
    path: string;
    body: any;
    headers: IncomingMessage["headers"];
  }[] = [];
  let health: unknown = {
    protocol: 1,
    instance_id: instanceID,
    binary_version: "0.1.0",
  };
  let healthHandler: ((response: ServerResponse) => void) | undefined;
  let rpc = async (_body: any, response: ServerResponse) => {
    response.setHeader("Content-Type", "application/json");
    response.end(JSON.stringify(page));
  };
  const server = createServer(async (request, response) => {
    let body = "";
    for await (const chunk of request) body += chunk;
    requests.push({
      path: request.url!,
      body: body ? JSON.parse(body) : undefined,
      headers: request.headers,
    });
    if (
      request.headers.authorization !== `Bearer ${token}` ||
      request.headers["x-scheduler-instance"] !== instanceID ||
      request.headers.origin
    ) {
      response.writeHead(401).end();
    } else if (request.url === "/v1/health") {
      if (healthHandler) healthHandler(response);
      else response.end(JSON.stringify(health));
    } else {
      await rpc(JSON.parse(body), response);
    }
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string")
    throw new Error("Missing fixture address");
  const endpoint = {
    protocol: 1,
    instance_id: instanceID,
    base_url: `http://127.0.0.1:${address.port}`,
    pid: process.pid,
    token,
  };
  await writeFile(
    configPath,
    JSON.stringify({
      protocol: 1,
      binary: join(home, "missing-binary"),
      data_home: home,
    }),
  );
  await writeFile(join(home, "endpoint.json"), JSON.stringify(endpoint));
  return {
    home,
    configPath,
    token,
    endpoint,
    requests,
    setHealth(value: unknown) {
      health = value;
    },
    setHealthHandler(value?: (response: ServerResponse) => void) {
      healthHandler = value;
    },
    setRpc(value: typeof rpc) {
      rpc = value;
    },
    async close() {
      server.closeAllConnections();
      await new Promise<void>((resolve, reject) =>
        server.close((error) => (error ? reject(error) : resolve())),
      );
      await rm(home, { recursive: true, force: true });
    },
  };
}
