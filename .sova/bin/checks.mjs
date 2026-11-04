// Hopp's checks for Sova's `test` verb, run through .sova/bin/test (toolchains on PATH).
// Selectors: none = the full run; an area (core, harness, tauri, ui, web, backend, fmt); a
// check's id; or core:test:<name>, tauri:test:<name>, backend:test:<package dir> for one
// filtered test run. Every selected check runs, in the table's order, even after a failure.
// SOVA_OUT gets one result per check, rewritten after each; full logs go to $SOVA_DATA/checks.
import { execFileSync, spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const root = process.env.SOVA_CHECKOUT || process.cwd();
const out = process.env.SOVA_OUT;
const logDir =
  process.env.SOVA_DATA ?
    path.join(process.env.SOVA_DATA, "checks")
  : fs.mkdtempSync(path.join(os.tmpdir(), "hopp-checks-"));
const TAIL_LINES = 20;

const node = (...args) => [process.execPath, ...args];
const yarn = (...args) => [process.execPath, process.env.HOPP_YARN_JS, ...args];
const cargo = (...args) => ["cargo", ...args];
const fmt = (id, cwd, areas) => ({ id, cwd, run: cargo("fmt", "--all", "--", "--check"), areas, full: true });

// `full`: in the run with no selectors. `areas`: the area selectors that pick it.
// `filter`: how a `<id>:<arg>` selector narrows the command. `stubs`: needs the files CI fakes.
const CHECKS = [
  { id: "versions", cwd: ".", run: node("scripts/check-tauri-versions.mjs"), areas: [], full: true },
  fmt("core:fmt", "core", ["core", "fmt"]),
  fmt("core:dev-fmt", "core/dev", ["core", "fmt"]),
  fmt("harness:fmt", "core/tests", ["harness", "fmt"]),
  fmt("tauri:fmt", "tauri/src-tauri", ["tauri", "fmt"]),
  { id: "backend:vet", cwd: "backend", run: ["go", "vet", "./..."], areas: ["backend"], full: true },
  {
    id: "backend:test",
    cwd: "backend",
    run: ["go", "test", "-tags=integration", "./..."],
    filter: (pkg) => ["go", "test", "-tags=integration", pkg.startsWith(".") ? pkg : `./${pkg}`],
    areas: ["backend"],
    full: true,
  },
  {
    id: "backend:lint",
    cwd: "backend",
    run: ["golangci-lint", "run", "--timeout=5m", "--config", "../.golangcli.yml"],
    areas: ["backend"],
    full: true,
  },
  { id: "ui:tsc", cwd: "tauri", run: yarn("tsc", "--noEmit", "--skipLibCheck"), areas: ["ui"], full: true },
  { id: "web:tsc", cwd: "web-app", run: yarn("tsc", "-b"), areas: ["web"], full: true },
  { id: "web:lint", cwd: "web-app", run: yarn("lint"), areas: ["web"], full: true },
  // core:test covers it; a fast smoke of the IPC protocol only.
  { id: "core:socket-lib", cwd: "core", run: cargo("test", "-p", "socket_lib"), areas: [], full: false },
  {
    id: "tauri:clippy",
    cwd: "tauri/src-tauri",
    run: cargo("clippy", "--all-targets", "--all-features", "--", "-D", "warnings"),
    stubs: true,
    areas: ["tauri"],
    full: true,
  },
  {
    id: "tauri:test",
    cwd: "tauri/src-tauri",
    run: cargo("test"),
    filter: (name) => cargo("test", name),
    stubs: true,
    areas: ["tauri"],
    full: true,
  },
  {
    id: "core:clippy",
    cwd: "core",
    run: cargo("clippy", "--all-features", "--", "-D", "warnings"),
    areas: ["core"],
    full: true,
  },
  {
    id: "core:test",
    cwd: "core",
    run: cargo("test", "--workspace", "--lib"),
    filter: (name) => cargo("test", "--workspace", "--lib", name),
    after: removeBadges,
    areas: ["core"],
    full: true,
  },
  {
    id: "core:clippy-tests",
    cwd: "core",
    run: cargo("clippy", "--lib", "--profile", "test", "--all-features", "--", "-D", "warnings"),
    areas: [],
    full: false,
  },
  { id: "harness:build", cwd: "core/tests", run: cargo("build"), areas: [], full: false },
];
const AREAS = [...new Set(CHECKS.flatMap((c) => c.areas))];

// Resolve selectors to [{check, key, run}] in table order; unknown selectors are failures.
function select(selectors) {
  const picked = new Map();
  const unknown = [];
  const add = (check, key = check.id, run = check.run) => {
    if (!picked.has(key)) picked.set(key, { check, key, run, order: picked.size });
  };
  if (selectors.length === 0) CHECKS.filter((c) => c.full).forEach((c) => add(c));
  for (const sel of selectors) {
    const byId = CHECKS.find((c) => c.id === sel);
    const filtered = CHECKS.find((c) => c.filter && sel.startsWith(`${c.id}:`) && sel.length > c.id.length + 1);
    if (byId) add(byId);
    else if (AREAS.includes(sel)) CHECKS.filter((c) => c.areas.includes(sel)).forEach((c) => add(c));
    else if (filtered) add(filtered, sel, filtered.filter(sel.slice(filtered.id.length + 1)));
    else unknown.push(sel);
  }
  const index = (c) => CHECKS.indexOf(c);
  const runs = [...picked.values()].sort((a, b) => index(a.check) - index(b.check) || a.order - b.order);
  return { runs, unknown };
}

// The files CI creates for the Tauri crate: tauri/dist and the core sidecar it bundles.
function ensureStubs() {
  fs.mkdirSync(path.join(root, "tauri/dist"), { recursive: true });
  const host = /^host: (\S+)$/m.exec(execFileSync("rustc", ["-vV"], { encoding: "utf8" }))?.[1];
  if (!host) throw new Error("rustc -vV named no host triple");
  const sidecar = path.join(root, "core/target/debug", `hopp_core-${host}`);
  if (!fs.existsSync(sidecar)) {
    fs.mkdirSync(path.dirname(sidecar), { recursive: true });
    fs.writeFileSync(sidecar, "");
  }
}

// The svg_renderer tests write badge PNGs into core/; delete the untracked ones.
function removeBadges() {
  const listed = execFileSync("git", ["ls-files", "--others", "-z", "--", ":(glob)core/*_badge*.png"], {
    cwd: root,
    encoding: "utf8",
  });
  for (const file of listed.split("\0").filter(Boolean)) fs.rmSync(path.join(root, file), { force: true });
}

function tail(file) {
  const lines = fs.readFileSync(file, "utf8").trimEnd().split("\n");
  return lines.slice(-TAIL_LINES).join("\n");
}

function runOne({ key, check, run }) {
  const log = path.join(logDir, `${key.replace(/[^A-Za-z0-9_.-]/g, "_")}.log`);
  return new Promise((resolve) => {
    const fd = fs.openSync(log, "w");
    fs.writeSync(fd, `$ (cd ${check.cwd} && ${run.join(" ")})\n`);
    let settled = false;
    const done = (code, error) => {
      if (settled) return;
      settled = true;
      if (error) fs.writeSync(fd, `${error}\n`);
      fs.closeSync(fd);
      resolve({ ok: code === 0, code, log });
    };
    let child;
    try {
      if (check.stubs) ensureStubs();
      child = spawn(run[0], run.slice(1), { cwd: path.join(root, check.cwd), stdio: ["ignore", fd, fd] });
    } catch (e) {
      return done(null, e.message);
    }
    child.on("error", (e) => done(null, e.message));
    child.on("close", (code, signal) => done(code, signal ? `killed by ${signal}` : undefined));
  });
}

const summary = { passed: 0, failed: 0, failures: [] };
function writeOut() {
  if (!out) return;
  fs.writeFileSync(`${out}.tmp`, `${JSON.stringify(summary, null, 2)}\n`);
  fs.renameSync(`${out}.tmp`, out);
}
function fail(name, message, count = "failed") {
  summary[count] = (summary[count] ?? 0) + 1;
  summary.failures.push({ name, message });
  writeOut();
  console.log(`FAILED ${name}\n${message}`);
}

fs.mkdirSync(logDir, { recursive: true });
const { runs, unknown } = select(process.argv.slice(2));
for (const sel of unknown) {
  fail(
    sel,
    `unknown selector; use an area (${AREAS.join(", ")}), a check (${CHECKS.map((c) => c.id).join(", ")}), or core:test:<name>, tauri:test:<name>, backend:test:<package dir>`,
  );
}
if (runs.length === 0) {
  fail("select", "no checks selected", "errors");
}
writeOut();

for (const r of runs) {
  const started = Date.now();
  console.log(`== ${r.key} (in ${r.check.cwd}): ${r.run.join(" ")}`);
  const res = await runOne(r);
  try {
    r.check.after?.();
  } catch (e) {
    console.log(`warning: cleanup after ${r.key} failed: ${e.message}`);
  }
  const secs = Math.round((Date.now() - started) / 1000);
  if (res.ok) {
    summary.passed += 1;
    writeOut();
    console.log(`ok ${r.key} (${secs}s)`);
  } else {
    fail(r.key, `exit ${res.code ?? "error"} after ${secs}s; full log: ${res.log}\n${tail(res.log)}`);
  }
}
console.log(`passed ${summary.passed}, failed ${summary.failed}; logs in ${logDir}`);
process.exitCode = summary.failed > 0 || summary.errors > 0 ? 1 : 0;
