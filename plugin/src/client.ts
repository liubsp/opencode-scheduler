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
    "Invalid scheduler input. Hint: use opencode-scheduler <command> --help for required arguments, UUIDs and limits.",
  invalid_time:
    "Invalid --at time. Hint: use 5:00 or 05:00:30 (next local occurrence on the daemon host), '2099-11-06 05:00' (host local date), or 2099-11-06T05:00:00+01:00 (explicit offset).",
  time_in_past:
    "The resolved appointment is no longer in the future. Hint: choose a future date, e.g. 2099-11-06T05:00:00+01:00 (explicit offset). A clock is not rolled forward again after validation; check schedule list before changing a lost-response request.",
  local_time_invalid:
    "Local time is ambiguous or nonexistent at a DST transition. Hint: choose another time or supply an explicit offset, e.g. 2099-11-06T05:00:00+01:00, to specify the intended instant.",
  timezone_unavailable:
    "The daemon host's local timezone could not be resolved. Hint: use an explicit offset, e.g. 2099-11-06T05:00:00+01:00, or repair the host timezone configuration.",
  missing_session:
    "Target session does not exist. Hint: verify the session ID or exact title in OpenCode; --session-title selects ALL exact matches.",
  opencode_unavailable:
    "OpenCode service is unavailable. Hint: run opencode-scheduler status and verify OpenCode is reachable. For a lost create response, reuse the same request ID and identical input.",
  unsupported_service:
    "OpenCode service is unsupported. Hint: use OpenCode V2 service version 2.2.0 or newer within major version 2; the CLI version may differ from the service.",
  unsupported_protocol:
    "Scheduler protocol is unsupported. Hint: upgrade the CLI, daemon and plugin together using the reviewed installer; preserve existing schedules.",
  storage_failure:
    "Scheduler storage operation failed; the cause is unknown. Hint: check free disk space and private data-directory access, then inspect diagnostic.log locally and run opencode-scheduler status. Do not delete the database. Reuse the same request ID and identical input after a lost create response.",
  storage_permission:
    "Scheduler storage access was denied or is read-only. Hint: check data-directory ownership and write permissions for the current user; do not bypass private ACLs or delete the database.",
  storage_full:
    "Scheduler storage has no free space. Hint: free disk space on the data volume without deleting scheduler data, then check opencode-scheduler status. Reuse the same request ID and identical input for a lost create response.",
  storage_busy:
    "Scheduler storage is locked by another writer. Hint: close external database writers and check opencode-scheduler status; preserve the database. For a lost create response reuse the same request ID and identical input.",
  private_directory_failure:
    "Private data-directory preparation or security validation failed. Hint: check ownership, permissions and free disk space; use a real directory, not a link. On Windows, use a filesystem with persistent ACL support. Do not weaken ACLs or delete existing scheduler data.",
  private_directory_timeout:
    "Private data-directory security helper exceeded its startup deadline. Hint: check host load and filesystem availability, then run opencode-scheduler status. Permissions were not bypassed; preserve scheduler data and reuse the same request ID and identical input for a lost create response.",
  daemon_startup_failure:
    "Scheduler daemon startup failed. Hint: inspect diagnostic.log locally and check private-directory access and free disk space; then run opencode-scheduler daemon start. Preserve scheduler data; reuse the same request ID and identical input for a lost create response.",
  daemon_startup_timeout:
    "Scheduler daemon startup exceeded its deadline. Hint: run opencode-scheduler status to see whether startup finished; inspect diagnostic.log locally if it did not. Reuse the same request ID and identical input for a lost create response.",
  daemon_unavailable:
    "Scheduler daemon connection failed; the operation may have been accepted. Hint: run opencode-scheduler status, then opencode-scheduler daemon start if absent. For create, reuse the same request ID and identical input; do not create a replacement request blindly.",
  authentication_failure:
    "Service authentication failed. Hint: verify the current user's OpenCode and scheduler registration and credentials; use the supported installer to repair registration. Do not bypass authentication or share credentials.",
  schedule_missing:
    "Schedule does not exist. Hint: run opencode-scheduler schedule list and use a schedule UUID from that list, not a request ID.",
  already_admitted:
    "Prompt is already admitted. Hint: cancellation cannot retract accepted work; inspect opencode-scheduler schedule show <id> and the target session. Admission does not mean agent completion.",
  outcome_uncertain:
    "Prompt admission is unconfirmed. Hint: inspect opencode-scheduler schedule log <id> and the target session before taking action. Cancellation cannot guarantee retraction; a new request could duplicate delivery.",
  request_conflict:
    "Request ID has different original input. Hint: replay with the same request ID and identical input. Use a new UUID only for an intentional separate appointment after checking schedule list.",
  resource_limit:
    "Scheduler resource limit exceeded. Hint: use --limit 1-100 for pages; reduce prompt size (maximum 65536 UTF-8 bytes) or narrow the target selection (maximum 100 recipients).",
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
                "Scheduler start/status helper did not complete. Hint: run opencode-scheduler status; the daemon may already be running. Inspect diagnostic.log locally if it is absent.",
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
