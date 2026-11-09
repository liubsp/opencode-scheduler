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
  invalid_input:
    "Invalid scheduler input. Hint: see opencode-scheduler <command> --help.",
  invalid_time:
    "Invalid --at time. Hint: use 5:00 (next local occurrence), '2099-11-06 05:00' (host local date), or 2099-11-06T05:00:00+01:00 (explicit offset).",
  time_in_past:
    "Appointment is no longer in the future; clocks are not rolled forward after validation. Hint: choose a future time. After a lost response, check schedule list before changing input.",
  local_time_invalid:
    "Local time falls in a DST gap or overlap. Hint: choose another time or specify an explicit offset, e.g. 2099-11-06T05:00:00+01:00.",
  timezone_unavailable:
    "Host timezone could not be resolved. Hint: use an explicit offset, e.g. 2099-11-06T05:00:00+01:00, or repair the host timezone.",
  missing_session:
    "Target session not found. Hint: check the ID or exact title in OpenCode; --session-title selects ALL exact matches.",
  opencode_unavailable:
    "OpenCode service is unavailable. Hint: run opencode-scheduler status and check OpenCode connectivity. After a lost create response, reuse the same request ID and identical input.",
  unsupported_service:
    "Unsupported OpenCode service. Hint: use V2 service version 2.2.0 or newer within major version 2; CLI versions may differ.",
  unsupported_protocol:
    "Unsupported scheduler protocol. Hint: upgrade the CLI, daemon and plugin together with the release installer.",
  storage_failure:
    "Storage failed; cause unknown. Hint: check free space and data-directory access; inspect diagnostic.log locally. Preserve the database. After a lost create response, reuse the same request ID and identical input.",
  storage_permission:
    "Storage access denied or read-only. Hint: check data-directory ownership and write permissions; preserve private access and the database.",
  storage_full:
    "Storage is full. Hint: free space without deleting scheduler data. After a lost create response, reuse the same request ID and identical input.",
  storage_busy:
    "Storage is locked by another writer. Hint: close external database writers. After a lost create response, reuse the same request ID and identical input.",
  private_directory_failure:
    "Private data-directory validation failed. Hint: check ownership, permissions and free space; use a real directory and, on Windows, a filesystem with persistent ACLs. Preserve data and private access.",
  private_directory_timeout:
    "Private-directory validation exceeded its deadline. Hint: check filesystem availability and host load, then run opencode-scheduler status. After a lost create response, reuse the same request ID and identical input.",
  daemon_startup_failure:
    "Scheduler startup failed. Hint: inspect diagnostic.log locally and check directory access and free space before running opencode-scheduler daemon start. After a lost create response, reuse the same request ID and identical input.",
  daemon_startup_timeout:
    "Scheduler startup timed out. Hint: run opencode-scheduler status; startup may have finished. If absent, inspect diagnostic.log locally. After a lost create response, reuse the same request ID and identical input.",
  daemon_unavailable:
    "Scheduler connection failed; the operation may have been accepted. Hint: run opencode-scheduler status, then daemon start if absent. For create, reuse the same request ID and identical input to avoid duplicates.",
  authentication_failure:
    "Service authentication failed. Hint: check the current user's OpenCode and scheduler registration and credentials locally; repair registration with the supported installer.",
  schedule_missing:
    "Schedule not found. Hint: use a schedule UUID from opencode-scheduler schedule list, not a request ID.",
  already_admitted:
    "Prompt already admitted; cancellation cannot retract it. Hint: inspect opencode-scheduler schedule show <id> and the target session. Admission is not agent completion.",
  outcome_uncertain:
    "Prompt admission is unconfirmed; cancellation cannot guarantee retraction. Hint: inspect opencode-scheduler schedule log <id> and the target session. A new request risks duplicate delivery.",
  request_conflict:
    "Request ID already has different input. Hint: replay identical input. Check schedule list before using a new UUID for a separate appointment.",
  resource_limit:
    "Scheduler limit exceeded. Hint: use --limit 1-100, prompts up to 65536 UTF-8 bytes and at most 100 recipients.",
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
        // The invoked CLI also performs a status RPC: 15s preparation + 30s startup
        // + 35s RPC, with 5s for process launch/exit. Readiness alone is not CLI completion.
        timeout: 85000,
        maxBuffer: 64 * 1024,
        signal,
        encoding: "utf8",
      },
      (error) =>
        error
          ? reject(
              new ClientError(
                "Scheduler start/status did not finish; the daemon may already be running. Hint: run opencode-scheduler status; if absent, inspect diagnostic.log locally.",
              ),
            )
          : resolve(),
    );
  });
}

function canonicalError(value: unknown): { code: string; message: string } {
  if (
    !object(value) ||
    !keys(value, ["code", "message"]) ||
    typeof value.code !== "string" ||
    typeof value.message !== "string" ||
    Buffer.byteLength(value.message) < 1 ||
    Buffer.byteLength(value.message) > 512 ||
    !Object.hasOwn(messages, value.code)
  )
    fail("Invalid scheduler response.");
  return { code: value.code, message: messages[value.code] };
}

function canonicalErrorField(record: unknown, field: string) {
  if (!object(record)) fail("Invalid scheduler response.");
  if (Object.hasOwn(record, field) && record[field] !== null)
    record[field] = canonicalError(record[field]);
}

function reply(status: number, value: unknown, op: string): Reply {
  if (!object(value)) fail("Invalid scheduler response.");
  if (
    value.ok === false &&
    keys(value, ["ok", "error"]) &&
    object(value.error)
  ) {
    return {
      ok: false,
      error: canonicalError(value.error),
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
  // Only the protocol's typed error slots are normalized; user prompt text is literal.
  const { kind, data } = value.result;
  switch (kind) {
    case "created":
    case "page":
    case "log": {
      const items = kind === "created" ? data.schedules : data.items;
      if (items !== undefined) {
        if (!Array.isArray(items)) fail("Invalid scheduler response.");
        for (const item of items)
          canonicalErrorField(item, kind === "log" ? "error" : "last_error");
      }
      break;
    }
    case "schedule":
      canonicalErrorField(data, "last_error");
      break;
    case "cancel":
      if (data.schedule !== undefined)
        canonicalErrorField(data.schedule, "last_error");
      break;
  }
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
