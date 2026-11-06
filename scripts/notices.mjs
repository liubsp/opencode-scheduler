import { execFileSync } from "node:child_process";
import { readdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const permissive =
  /^(?:MIT|Apache-2\.0|Unicode-3\.0|BSD-3-Clause|Zlib|BSL-1\.0|Unlicense|ISC)(?:(?: OR | AND |\/)(?:MIT|Apache-2\.0|Unicode-3\.0|BSD-3-Clause|Zlib|BSL-1\.0|Unlicense|ISC))*$/;
async function notice(directory, name, license) {
  if (!permissive.test(license.replace(/[()]/g, "")))
    throw new Error(`Unreviewed license: ${name}: ${license}`);
  const files = (await readdir(directory))
    .filter((n) =>
      /^(?:license|copying|notice|unlicense)(?:[._-].*)?$/i.test(n),
    )
    .sort();
  if (!files.length) throw new Error(`No license text: ${name}`);
  const bodies = await Promise.all(
    files.map(
      async (n) => `${n}\n${await readFile(join(directory, n), "utf8")}`,
    ),
  );
  return `\n===== ${name} (${license}) =====\n${bodies.join("\n")}\n`;
}

export async function nodeNotices(root, metadata) {
  const incorporated = new Set();
  for (const output of Object.values(metadata.outputs)) {
    for (const [input, info] of Object.entries(output.inputs)) {
      if (!info.bytesInOutput || !input.includes("node_modules/")) continue;
      const prefix = input.slice(
        0,
        input.lastIndexOf("node_modules/") + "node_modules/".length,
      );
      const rest = input.slice(prefix.length).split("/");
      const name = rest[0].startsWith("@")
        ? rest.slice(0, 2).join("/")
        : rest[0];
      // Only the inspected SDK identity helper is approved to be incorporated.
      if (
        name !== "@opencode/plugin" ||
        !input.endsWith("/dist/promise/plugin.js")
      )
        throw new Error(`Unreviewed bundled contribution: ${input}`);
      incorporated.add(prefix + name);
    }
  }
  let text =
    "Third-party code incorporated into the bundled plugin. Node builtins are supplied by the host.\n";
  for (const path of [...incorporated].sort()) {
    const directory = join(root, path);
    const pkg = JSON.parse(
      await readFile(join(directory, "package.json"), "utf8"),
    );
    // The published SDK omits its LICENSE; retain the exact v2.2.0 upstream text.
    if (
      pkg.name !== "@opencode/plugin" ||
      pkg.version !== "2.2.0" ||
      pkg.license !== "MIT"
    )
      throw new Error("Review SDK license before updating");
    text += `\n===== ${pkg.name} ${pkg.version} (MIT) =====\nSource: https://github.com/anomalyco/opencode/blob/v2.2.0/LICENSE\n${await readFile(join(root, "scripts/licenses/opencode-plugin-2.2.0.txt"), "utf8")}\n`;
  }
  return text;
}

export async function rustNotices(root) {
  const run = (command, args) =>
    execFileSync(command, args, {
      cwd: root,
      encoding: "utf8",
      maxBuffer: 32 * 1024 * 1024,
      windowsHide: true,
    });
  const metadata = JSON.parse(
    run("cargo", [
      "metadata",
      "--locked",
      "--format-version",
      "1",
      "--filter-platform",
      "x86_64-pc-windows-msvc",
    ]),
  );
  const packages = new Map(metadata.packages.map((p) => [p.id, p]));
  const nodes = new Map(metadata.resolve.nodes.map((n) => [n.id, n]));
  const included = new Set();
  const visit = (id) => {
    const pkg = packages.get(id);
    if (
      included.has(id) ||
      pkg.targets.some((t) => t.kind.includes("proc-macro"))
    )
      return;
    included.add(id);
    for (const dep of nodes.get(id).deps)
      if (dep.dep_kinds.some((k) => k.kind === null)) visit(dep.pkg);
  };
  visit(metadata.resolve.root);
  let text =
    "Third-party runtime crates for the locked Windows x64 build (normal dependency graph; excludes dev/build-only and proc-macro dependencies). License texts include upstream supplemental notices.\n";
  for (const id of [...included].sort()) {
    if (id === metadata.resolve.root) continue;
    const pkg = packages.get(id);
    text += await notice(
      dirname(pkg.manifest_path),
      `${pkg.name} ${pkg.version}`,
      pkg.license,
    );
    if (pkg.name === "libsqlite3-sys") {
      const sqlite = await readFile(
        join(dirname(pkg.manifest_path), "sqlite3/sqlite3.c"),
        "utf8",
      );
      const attribution = [
        ...sqlite.slice(0, 8192).matchAll(/\/\*[\s\S]*?\*\//g),
      ].find((match) =>
        match[0].includes("The author disclaims copyright"),
      )?.[0];
      if (!attribution)
        throw new Error("Missing bundled SQLite public-domain notice");
      text += `\nBundled SQLite (public domain)\n${attribution}\n`;
    }
  }
  const sysroot = run("rustc", ["--print", "sysroot"]).trim();
  const doc = join(sysroot, "share/doc/rust");
  // Preserve Rust's published attribution rather than inventing a std inventory.
  // It covers all std targets, including dependencies not linked by this MSVC build.
  text += `\n===== Rust standard library =====\nRust supplies the following cross-target attribution appendix as COPYRIGHT-library.html. Entries for other targets are upstream notices, not additional runtime dependencies of this Windows MSVC payload. The standard library is used under its MIT/Apache terms.\n${await readFile(join(doc, "COPYRIGHT-library.html"), "utf8")}\n`;
  // These permissive texts cover std and its MSVC runtime exceptions; compiler,
  // documentation fonts and GNU runtime license files are not shipped here.
  for (const name of [
    "Apache-2.0.txt",
    "MIT.txt",
    "Unicode-3.0.txt",
    "BSD-2-Clause.txt",
    "ISC.txt",
    "LLVM-exception.txt",
    "NCSA.txt",
  ]) {
    text += `\nRust ${name}\n${await readFile(join(doc, "licenses", name), "utf8")}\n`;
  }
  return text;
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  const root = fileURLToPath(new URL("../", import.meta.url));
  if (!process.argv[2])
    throw new Error("Usage: node scripts/notices.mjs <output-file>");
  await writeFile(process.argv[2], await rustNotices(root));
}
