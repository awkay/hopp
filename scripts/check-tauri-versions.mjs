#!/usr/bin/env node
// Fails when an @tauri-apps/* npm package and its Rust crate differ in major.minor.
//
// `tauri build` (tauri-cli `check_mismatched_packages`) refuses such a pair with "Found version
// mismatched Tauri packages", but tsc, clippy and cargo test don't notice, and CI never runs
// `tauri build`, so a bad pin otherwise shows up only at release time. tauri-cli compares
// `@tauri-apps/api` with the `tauri` crate and `@tauri-apps/plugin-<name>` with
// `tauri-plugin-<name>`. Its npm side is `yarn info --json` run in `tauri/`, which lists the
// versions that workspace's direct dependencies resolve to; its crate side is the single
// Cargo.lock entry of that name. This script reads the same versions straight from the lockfiles:
// the `tauri` workspace entry in yarn.lock names each dependency's descriptor (e.g.
// `@tauri-apps/api@npm:~2.10`), and only the lock entry keyed by that descriptor counts. Other
// resolutions of the same package (other workspaces, transitive `^2.8.0` ranges) are ignored.
//
// Node built-ins only, no install needed. Usage:
//   node scripts/check-tauri-versions.mjs [--yarn-lock <path>] [--cargo-lock <path>]
//                                         [--workspace <dir>]
// Defaults are this repo's yarn.lock, tauri/src-tauri/Cargo.lock and the `tauri` workspace; the
// YARN_LOCK and CARGO_LOCK environment variables also override the paths.
// Exit codes: 0 aligned, 1 mismatch, 2 bad input (missing file, workspace or lock entry).

import { readFileSync } from "node:fs";
import { relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));

function fail(message) {
  console.error(`error: ${message}`);
  process.exit(2);
}

const usage =
  "usage: node scripts/check-tauri-versions.mjs [--yarn-lock <path>] [--cargo-lock <path>] [--workspace <dir>]";
let args;
try {
  args = parseArgs({
    options: {
      "yarn-lock": { type: "string" },
      "cargo-lock": { type: "string" },
      workspace: { type: "string", default: "tauri" },
      help: { type: "boolean", short: "h" },
    },
  }).values;
} catch (error) {
  fail(`${error.message}\n${usage}`);
}
if (args.help) {
  console.log(usage);
  process.exit(0);
}

const yarnLockPath = resolve(args["yarn-lock"] ?? process.env.YARN_LOCK ?? resolve(repoRoot, "yarn.lock"));
const cargoLockPath = resolve(
  args["cargo-lock"] ?? process.env.CARGO_LOCK ?? resolve(repoRoot, "tauri/src-tauri/Cargo.lock"),
);
const workspace = args.workspace;

function read(path) {
  try {
    return readFileSync(path, "utf8");
  } catch (error) {
    return fail(`cannot read ${path}: ${error.message}`);
  }
}

function display(path) {
  const fromCwd = relative(process.cwd(), path);
  return fromCwd && !fromCwd.startsWith("..") ? fromCwd : path;
}

// Yarn Berry lockfiles are a YAML subset: each entry starts at column 0 with a key listing one or
// more descriptors (`"a@npm:^1, a@npm:^1.2":`), fields are indented two spaces and the
// `dependencies` map four. Quoted strings use JSON escaping.
function unquote(text) {
  return text.startsWith('"') ? JSON.parse(text) : text;
}

function parseYarnLock(text) {
  const entries = [];
  let entry = null;
  let inDependencies = false;
  for (const line of text.split(/\r?\n/)) {
    if (line === "" || line.startsWith("#")) continue;
    if (!line.startsWith(" ")) {
      const key = unquote(line.replace(/:$/, ""));
      entry = { descriptors: key.split(", "), fields: {}, dependencies: {} };
      entries.push(entry);
      inDependencies = false;
      continue;
    }
    if (!entry) continue;
    const field = line.match(/^ {2}([^\s:]+):(?: (.*))?$/);
    if (field) {
      inDependencies = field[1] === "dependencies";
      if (field[2] !== undefined) entry.fields[field[1]] = unquote(field[2]);
      continue;
    }
    const dependency = inDependencies && line.match(/^ {4}("(?:[^"\\]|\\.)*"|[^\s:]+): (.*)$/);
    if (dependency) entry.dependencies[unquote(dependency[1])] = unquote(dependency[2]);
  }
  return entries;
}

function parseCargoLock(text) {
  const crates = new Map();
  for (const block of text.split(/^\[\[package\]\]$/m).slice(1)) {
    const name = block.match(/^name = "([^"]+)"$/m)?.[1];
    const version = block.match(/^version = "([^"]+)"$/m)?.[1];
    if (!name || !version) continue;
    if (!crates.has(name)) crates.set(name, []);
    crates.get(name).push(version);
  }
  return crates;
}

function majorMinor(version) {
  const match = version.match(/^(\d+)\.(\d+)\./);
  return match ? `${match[1]}.${match[2]}` : null;
}

// npm name -> crate name, the same pairing tauri-cli uses.
function crateFor(npmName) {
  if (npmName === "@tauri-apps/api") return "tauri";
  const plugin = npmName.match(/^@tauri-apps\/plugin-(.+)$/);
  return plugin ? `tauri-plugin-${plugin[1]}` : null;
}

const yarnEntries = parseYarnLock(read(yarnLockPath));
const crates = parseCargoLock(read(cargoLockPath));

const workspaceEntry = yarnEntries.find((e) => e.fields.resolution?.endsWith(`@workspace:${workspace}`));
if (!workspaceEntry) fail(`no "<name>@workspace:${workspace}" entry in ${display(yarnLockPath)}`);

const byDescriptor = new Map();
for (const entry of yarnEntries) {
  for (const descriptor of entry.descriptors) byDescriptor.set(descriptor, entry);
}

const npmPackages = new Map(); // npm name -> resolved version, for the workspace's direct deps
for (const [name, range] of Object.entries(workspaceEntry.dependencies)) {
  if (!crateFor(name)) continue;
  const descriptor = `${name}@${range}`;
  const version = byDescriptor.get(descriptor)?.fields.version;
  if (!version) fail(`${display(yarnLockPath)} has no entry for ${descriptor}; run \`yarn install\``);
  npmPackages.set(name, version);
}

const pairedCrates = new Set();
const aligned = [];
const mismatched = [];
const npmOnly = [];
for (const [npmName, npmVersion] of [...npmPackages].sort(([a], [b]) => a.localeCompare(b))) {
  const crateName = crateFor(npmName);
  const crateVersions = crates.get(crateName);
  if (!crateVersions) {
    npmOnly.push(`${npmName} ${npmVersion}`);
    continue;
  }
  pairedCrates.add(crateName);
  if (crateVersions.length > 1) {
    // Cargo's `links` key should make this impossible for Tauri crates; tauri-cli would then
    // compare the Cargo.toml requirement instead of a locked version.
    mismatched.push(
      `${npmName} ${npmVersion}: ${display(cargoLockPath)} holds several ${crateName} versions ` +
        `(${crateVersions.join(", ")}); keep one`,
    );
    continue;
  }
  const crateVersion = crateVersions[0];
  const npmMinor = majorMinor(npmVersion);
  const crateMinor = majorMinor(crateVersion);
  if (npmMinor && npmMinor === crateMinor) {
    aligned.push(`${npmName} ${npmVersion} <-> ${crateName} ${crateVersion}`);
  } else {
    mismatched.push(
      `${npmName} ${npmVersion} vs ${crateName} ${crateVersion}: pin "${npmName}": "~${crateMinor}" ` +
        `in ${workspace}/package.json, or move the crate to ${npmMinor}`,
    );
  }
}

const crateOnly = [...crates]
  .filter(([name]) => (name === "tauri" || name.startsWith("tauri-plugin-")) && !pairedCrates.has(name))
  .map(([name, versions]) => `${name} ${versions.join(", ")}`)
  .sort();

const sources = `workspace "${workspace}" in ${display(yarnLockPath)} vs ${display(cargoLockPath)}`;
console.log(`Tauri npm/crate major.minor: ${sources}`);
for (const line of aligned) console.log(`  ok        ${line}`);
for (const line of mismatched) console.log(`  MISMATCH  ${line}`);
if (npmOnly.length) console.log(`info: npm packages with no crate (not checked): ${npmOnly.join("; ")}`);
if (crateOnly.length) console.log(`info: crates with no npm package (not checked): ${crateOnly.join("; ")}`);

if (mismatched.length) {
  console.error(
    `error: ${mismatched.length} Tauri package(s) differ in major.minor between npm and Rust; ` +
      '`tauri build` refuses this ("Found version mismatched Tauri packages").',
  );
  process.exit(1);
}
