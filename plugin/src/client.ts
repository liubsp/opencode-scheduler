import { execFile } from "node:child_process";
import { access, open } from "node:fs/promises";
import { Agent, request } from "node:http";
import { isAbsolute, join } from "node:path";

type Install = { protocol: 1; binary: string; data_home: string };
type Endpoint = {
  protocol: 1;
  instance_id: string;
  base_url: string;
  pid: number;
  token: string;
};
export type Reply =
  | { ok: true; result: { kind: string; data: any } }
  | { ok: false; error: { code: string; message: string } };
export const uuidPattern =
  "^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-4[0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}$";
const uuid = new RegExp(uuidPattern);
const kinds: Record<string, string> = {
  schedule_create: "created",
  schedule_list: "page",
  schedule_show: "schedule",
  schedule_cancel: "cancel",
  schedule_log: "log",
  status: "status",
};
const messages: Record<string, string> = {
  invalid_input: "Invalid scheduler input.",
  missing_session: "Target session does not exist.",
  opencode_unavailable: "OpenCode service is unavailable.",
  unsupported_service: "OpenCode service is unsupported.",
  unsupported_protocol: "Scheduler protocol is unsupported.",
  storage_failure: "Scheduler storage operation failed.",
  authentication_failure: "Service authentication failed.",
  schedule_missing: "Schedule does not exist.",
  already_admitted: "Prompt is already admitted.",
  outcome_uncertain: "Prompt admission is unconfirmed.",
  request_conflict: "Request ID has different original input.",
  resource_limit: "Scheduler resource limit exceeded.",
};
class ClientError extends Error {}
function fail(message: string): never {
  throw new ClientError(message);
}
function object(value: unknown): value is Record<string, any> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function keys(value: Record<string, any>, expected: string[]) {
  return (
    Object.keys(value).length === expected.length &&
    expected.every((key) => Object.hasOwn(value, key))
  );
}
function cancelled(): never {
  throw new DOMException(
    "Scheduler wait cancelled; accepted schedules are unchanged.",
    "AbortError",
  );
}
function check(signal: AbortSignal) {
  if (signal.aborted) cancelled();
}

async function readJSON(path: string | URL, signal: AbortSignal): Promise<any> {
  check(signal);
  const file = await open(path, "r");
  try {
    // Read one bounded buffer, even if the file grows after opening.
    const buffer = Buffer.alloc(64 * 1024 + 1);
    let size = 0;
    while (size < buffer.length) {
      check(signal);
      const { bytesRead } = await file.read(
        buffer,
        size,
        buffer.length - size,
        null,
      );
      if (bytesRead === 0) break;
      size += bytesRead;
    }
    if (size > 64 * 1024) fail(messages.resource_limit);
    return JSON.parse(buffer.subarray(0, size).toString("utf8"));
  } finally {
    await file.close();
  }
}

function validateInstall(value: unknown): Install {
  if (
    !object(value) ||
    !keys(value, ["protocol", "binary", "data_home"]) ||
    value.protocol !== 1 ||
    typeof value.binary !== "string" ||
    !isAbsolute(value.binary) ||
    typeof value.data_home !== "string" ||
    !isAbsolute(value.data_home) ||
    value.binary.includes("\0") ||
    value.data_home.includes("\0")
  ) {
    fail("Invalid scheduler installation configuration.");
  }
  return value as Install;
}
function validateEndpoint(value: unknown): Endpoint {
  if (
    !object(value) ||
    !keys(value, ["protocol", "instance_id", "base_url", "pid", "token"])
  )
    fail("Invalid scheduler endpoint descriptor.");
  if (value.protocol !== 1) fail(messages.unsupported_protocol);
  if (
    typeof value.instance_id !== "string" ||
    !uuid.test(value.instance_id) ||
    !Number.isInteger(value.pid) ||
    value.pid < 1 ||
    value.pid > 0xffffffff ||
    typeof value.token !== "string" ||
    !/^[a-fA-F0-9]{64,128}$/.test(value.token) ||
    // A literal canonical IPv4 authority prevents URL parser normalization, DNS and credentials.
    typeof value.base_url !== "string" ||
    !/^http:\/\/127\.0\.0\.1:[1-9][0-9]{0,4}$/.test(value.base_url) ||
    Number(value.base_url.split(":").at(-1)) > 65535
  )
    fail("Invalid scheduler endpoint descriptor.");
  return value as Endpoint;
}

async function httpJSON(
  endpoint: Endpoint,
  path: string,
  body: string | undefined,
  signal: AbortSignal,
  max: number,
  timeout: number,
): Promise<{ status: number; value: any }> {
  check(signal);
  return new Promise((resolve, reject) => {
    let size = 0;
    const chunks: Buffer[] = [];
    // Avoid the host's globalAgent: newer Node versions can enable environment proxies there.
    // This dedicated non-keepalive agent has no proxy configuration. Never send Origin.
    const req = request(
      `${endpoint.base_url}${path}`,
      {
        method: body === undefined ? "GET" : "POST",
        agent: new Agent({ keepAlive: false }),
        signal,
        headers: {
          Authorization: `Bearer ${endpoint.token}`,
          "X-Scheduler-Instance": endpoint.instance_id,
          ...(body === undefined
            ? {}
            : {
                "Content-Type": "application/json",
                "Content-Length": Buffer.byteLength(body),
              }),
        },
      },
      (response) => {
        if (response.statusCode! >= 300 && response.statusCode! < 400) {
          req.destroy(new ClientError("Scheduler redirects are not allowed."));
          response.destroy();
          return;
        }
        if (Number(response.headers["content-length"]) > max) {
          req.destroy(new ClientError(messages.resource_limit));
          response.destroy();
          return;
        }
        response.on("error", reject);
        response.on("data", (chunk: Buffer) => {
          size += chunk.length;
          if (size > max) {
            req.destroy(new ClientError(messages.resource_limit));
            response.destroy();
          } else chunks.push(chunk);
        });
        response.on("end", () => {
          try {
            resolve({
              status: response.statusCode!,
              value: JSON.parse(Buffer.concat(chunks).toString("utf8")),
            });
          } catch {
            reject(new ClientError("Invalid scheduler response."));
          }
        });
      },
    );
    const timer = setTimeout(
      () => req.destroy(new ClientError("Scheduler request timed out.")),
      timeout,
    );
    req.on("upgrade", (response, socket) => {
      reject(new ClientError("Scheduler HTTP upgrades are not supported."));
      response.destroy();
      socket.destroy();
      req.destroy();
    });
    req.on("close", () => {
      clearTimeout(timer);
      // Successful responses and transport errors settle first; close alone must not hang.
      reject(
        new ClientError(
          "Scheduler connection closed before response completed.",
        ),
      );
    });
    req.on("error", reject);
    req.end(body);
  });
}

async function discover(
  config: Install,
  signal: AbortSignal,
): Promise<Endpoint | undefined> {
  let value: unknown;
  try {
    value = await readJSON(join(config.data_home, "endpoint.json"), signal);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw error;
  }
  const endpoint = validateEndpoint(value);
  let health;
  try {
    health = await httpJSON(
      endpoint,
      "/v1/health",
      undefined,
      signal,
      64 * 1024,
      2000,
    );
  } catch (error) {
    // Only dead listeners mean absent; a live but unhealthy daemon must not be started over.
    if ((error as NodeJS.ErrnoException).code === "ECONNREFUSED")
      return undefined;
    throw error;
  }
  if (health.status < 200 || health.status >= 300)
    fail(messages.authentication_failure);
  if (
    !object(health.value) ||
    health.value.protocol !== 1 ||
    health.value.instance_id !== endpoint.instance_id ||
    typeof health.value.binary_version !== "string"
  )
    fail(messages.unsupported_protocol);
  return endpoint;
}

async function start(config: Install, signal: AbortSignal): Promise<void> {
  check(signal);
  await new Promise<void>((resolve, reject) => {
    execFile(
      config.binary,
      ["daemon", "start"],
      {
        env: { ...process.env, OPENCODE_SCHEDULER_HOME: config.data_home },
        windowsHide: true,
        shell: false,
        timeout: 20000,
        maxBuffer: 64 * 1024,
        signal,
        encoding: "utf8",
      },
      (error) =>
        error
          ? reject(new ClientError("Scheduler daemon start failed."))
          : resolve(),
    );
  });
}

function reply(status: number, value: unknown, op: string): Reply {
  if (!object(value)) fail("Invalid scheduler response.");
  if (
    value.ok === false &&
    keys(value, ["ok", "error"]) &&
    object(value.error) &&
    keys(value.error, ["code", "message"]) &&
    typeof value.error.code === "string" &&
    typeof value.error.message === "string" &&
    Buffer.byteLength(value.error.message) >= 1 &&
    Buffer.byteLength(value.error.message) <= 512 &&
    Object.hasOwn(messages, value.error.code)
  ) {
    return {
      ok: false,
      error: { code: value.error.code, message: messages[value.error.code] },
    };
  }
  if (
    status < 200 ||
    status >= 300 ||
    !keys(value, ["ok", "result"]) ||
    value.ok !== true ||
    !object(value.result) ||
    !keys(value.result, ["kind", "data"]) ||
    value.result.kind !== kinds[op] ||
    !object(value.result.data)
  )
    fail("Invalid scheduler response.");
  return value as Reply;
}

/** No scheduling state: only a lifetime signal for this plugin instance's own waits. */
export class SchedulerClient {
  private readonly lifetime = new AbortController();
  constructor(
    private readonly installPath: string | URL = new URL(
      "../install.json",
      import.meta.url,
    ),
  ) {}
  dispose(): void {
    this.lifetime.abort();
  }
  status(signal?: AbortSignal): Promise<Reply> {
    return this.call("status", {}, signal);
  }
  async call(op: string, input: unknown, caller?: AbortSignal): Promise<Reply> {
    const signal = AbortSignal.any([
      this.lifetime.signal,
      ...(caller ? [caller] : []),
    ]);
    check(signal);
    if (!Object.hasOwn(kinds, op)) fail("Unsupported scheduler operation.");
    if (
      op === "schedule_create" &&
      (!object(input) ||
        typeof input.request_id !== "string" ||
        !uuid.test(input.request_id))
    )
      fail("A stable creation request_id UUID v4 is required.");
    const requestID =
      op === "schedule_create"
        ? ((input as Record<string, unknown>).request_id as string)
        : undefined;
    // Freeze bytes before the first await, including the UUID and literal prompt.
    const body = JSON.stringify({ op, input });
    if (Buffer.byteLength(body) > 256 * 1024) fail(messages.resource_limit);
    try {
      const config = validateInstall(await readJSON(this.installPath, signal));
      let endpoint = await discover(config, signal);
      if (!endpoint && op === "status") {
        const binary = await access(config.binary).then(
          () => true,
          () => false,
        );
        check(signal);
        return {
          ok: true,
          result: {
            kind: "status",
            data: {
              installed: { binary, plugin: true },
              daemon: { reachable: false, protocol_compatible: false },
              opencode: { state: "unknown", service_version: null },
              schedules: {
                pending: null,
                blocked: null,
                failed: null,
                admission_unconfirmed: null,
              },
            },
          },
        };
      }
      if (!endpoint) {
        await start(config, signal);
        endpoint = await discover(config, signal);
      }
      if (!endpoint) fail("Scheduler daemon is unavailable.");
      const response = await httpJSON(
        endpoint,
        "/v1/rpc",
        body,
        signal,
        16 * 1024 * 1024,
        35000,
      );
      check(signal);
      const result = reply(response.status, response.value, op);
      // Malformed authenticated peers must not turn a tool result into a credential disclosure.
      if (JSON.stringify(result).includes(endpoint.token))
        fail("Invalid scheduler response.");
      return result;
    } catch (error) {
      if (signal.aborted) cancelled();
      const message =
        error instanceof ClientError
          ? error.message
          : "Scheduler operation failed.";
      const hint =
        op === "schedule_create"
          ? ` Request ${requestID} may have been accepted; retry only with the same request_id and identical input.`
          : "";
      throw new Error(message + hint);
    }
  }
}
