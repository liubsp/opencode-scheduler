import { lstat, readdir, readFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createRequire, isBuiltin } from "node:module";
import { fileURLToPath, pathToFileURL } from "node:url";

const pluginFiles = [
  "dist/index.js",
  "package.json",
  "LICENSE",
  "THIRD-PARTY-NOTICES.txt",
];
const payloadFiles = [
  "bin/opencode-scheduler.exe",
  ...pluginFiles.map((n) => `plugin/${n}`),
  "skills/opencode-scheduler/SKILL.md",
  "README.md",
  "LICENSE",
  "THIRD-PARTY-NOTICES.txt",
];
const require = createRequire(
  new URL("../plugin/package.json", import.meta.url),
);
const ts = require("typescript");

function checkImports(text) {
  const source = ts.createSourceFile(
    "index.js",
    text,
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.JS,
  );
  if (source.parseDiagnostics.length)
    throw new Error("Invalid bundled JavaScript");
  const check = (specifier) => {
    if (
      !specifier ||
      !ts.isStringLiteralLike(specifier) ||
      !isBuiltin(specifier.text)
    )
      throw new Error("Non-builtin runtime import forbidden");
  };
  const visit = (node) => {
    if (
      ts.isImportDeclaration(node) ||
      (ts.isExportDeclaration(node) && node.moduleSpecifier)
    )
      check(node.moduleSpecifier);
    if (
      ts.isCallExpression(node) &&
      (node.expression.kind === ts.SyntaxKind.ImportKeyword ||
        (ts.isIdentifier(node.expression) &&
          /^(?:require|__require)$/.test(node.expression.text)))
    ) {
      if (node.arguments.length !== 1)
        throw new Error("Invalid runtime import");
      check(node.arguments[0]);
    }
    ts.forEachChild(node, visit);
  };
  visit(source);
}

function checkNative(bytes) {
  if (bytes.length < 64 || bytes.toString("ascii", 0, 2) !== "MZ")
    throw new Error("Expected Windows x64 executable");
  const pe = bytes.readUInt32LE(60);
  const optional = pe + 24;
  if (
    optional + 240 > bytes.length ||
    bytes.toString("ascii", pe, pe + 4) !== "PE\0\0" ||
    bytes.readUInt16LE(pe + 4) !== 0x8664 ||
    bytes.readUInt16LE(optional) !== 0x20b
  )
    throw new Error("Expected Windows x64 PE header");
  const debug = optional + 112 + 6 * 8;
  if (bytes.readUInt32LE(debug) || bytes.readUInt32LE(debug + 4))
    throw new Error("PE debug directory forbidden");
}

export async function scan(
  directory,
  { plugin = false, installer = false } = {},
) {
  const expected = new Set(
    plugin
      ? pluginFiles
      : [...payloadFiles, ...(installer ? ["install.ps1"] : [])],
  );
  const directories = new Set(
    [...expected].flatMap((n) => {
      const pieces = n.split("/");
      return pieces
        .slice(0, -1)
        .map((_, i) => pieces.slice(0, i + 1).join("/"));
    }),
  );
  const root = resolve(fileURLToPath(new URL("../", import.meta.url)));
  const prefixes = [
    root,
    homedir(),
    tmpdir(),
    process.env.CARGO_HOME,
    process.env.RUSTUP_HOME,
    process.env.TEMP,
    process.env.TMP,
    ...(process.env.SCHEDULER_PRIVATE_PREFIXES || "").split("|"),
  ]
    .filter((n) => n && n.length > 3)
    .flatMap((n) => [n, n.replaceAll("\\", "/"), n.replaceAll("\\", "\\\\")])
    .map((n) => n.toLowerCase());
  const walk = async (relative = "") => {
    for (const name of await readdir(join(directory, relative))) {
      const path = relative ? `${relative}/${name}` : name;
      const stat = await lstat(join(directory, path));
      if (stat.isSymbolicLink()) throw new Error(`Link forbidden: ${path}`);
      if (stat.isDirectory()) {
        if (!directories.has(path))
          throw new Error(`Unexpected directory: ${path}`);
        await walk(path);
        continue;
      }
      if (!stat.isFile() || !expected.delete(path))
        throw new Error(`Unexpected file: ${path}`);
      const bytes = await readFile(join(directory, path));
      if (path.endsWith(".js")) checkImports(bytes.toString("utf8"));
      if (path.endsWith(".exe")) checkNative(bytes);
      const swapped = Buffer.from(
        bytes.subarray(0, bytes.length - (bytes.length % 2)),
      ).swap16();
      for (const text of [
        bytes.toString("utf8"),
        bytes.toString("utf16le"),
        swapped.toString("utf16le"),
      ]) {
        const lower = text.toLowerCase();
        if (prefixes.some((n) => lower.includes(n)))
          throw new Error(`Private build prefix: ${path}`);
        if (
          /(?:[a-z]:[\\/](?:users|dev|actions|a)[\\/]|\/(?:home|users)\/[^/\s]+\/|sourceMappingURL|\.pdb\b|RSDS|Bearer\s+[A-Za-z0-9._-]{20,}|"token"\s*:\s*"[^"\s]{20,}"|-----BEGIN (?:RSA |OPENSSH )?PRIVATE KEY-----)/i.test(
            text,
          )
        )
          throw new Error(`Private/debug content: ${path}`);
      }
    }
  };
  await walk();
  if (expected.size)
    throw new Error(`Missing files: ${[...expected].join(", ")}`);
  const pkg = JSON.parse(
    await readFile(
      join(directory, plugin ? "package.json" : "plugin/package.json"),
      "utf8",
    ),
  );
  if (
    [
      "dependencies",
      "optionalDependencies",
      "peerDependencies",
      "bundledDependencies",
      "bundleDependencies",
    ].some((field) => Object.keys(pkg[field] || {}).length)
  )
    throw new Error("Runtime package dependencies forbidden");
  console.log(
    `Audited ${plugin ? "plugin" : "payload"} directory (${directory === resolve(directory) ? "absolute input" : "relative input"}).`,
  );
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  if (!process.argv[2])
    throw new Error(
      "Usage: node scripts/check-package.mjs <payload-directory> [--installer|--plugin]",
    );
  await scan(process.argv[2], {
    plugin: process.argv.includes("--plugin"),
    installer: process.argv.includes("--installer"),
  });
}
