#!/usr/bin/env node
/**
 * spec-guard — the mechanism that keeps Spec-Driven Development honest.
 *
 * A spec suite that nobody checks is documentation, and documentation drifts. This script turns
 * the specs into a build gate by enforcing four rules:
 *
 *   1. Every requirement declared in `specs/SPEC-*.md` is covered by at least one test that
 *      says so, with a `Covers: RQ-XXX-NNN` annotation.
 *   2. Every `Covers:` annotation names a requirement that actually exists — so deleting a
 *      requirement without deleting its test is caught, and so is a typo.
 *   3. A handful of structural claims the specs make about the repository are true:
 *      the release profile aborts on panic, `unsafe` is denied, the Tauri CSP is strict, and
 *      GitHub Actions are pinned by commit SHA.
 *   4. Requirement IDs are unique across the whole suite.
 *
 * Run it with `--write` to rewrite each spec's "Covered by" column from the annotations that
 * actually exist, so the tables can never quietly disagree with the code.
 *
 * No dependencies: it runs on a bare Node install, in CI and on a fresh clone alike.
 */

import { readFileSync, writeFileSync, readdirSync, statSync, existsSync } from "node:fs";
import { join, relative, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SPEC_DIR = join(ROOT, "specs");
const WRITE = process.argv.includes("--write");

const SOURCE_ROOTS = ["crates", "apps", "scripts", ".github", "docs"];
const SOURCE_EXTENSIONS = [".rs", ".ts", ".vue", ".mjs", ".js", ".yml", ".yaml", ".md", ".toml"];
const SKIP_DIRS = new Set(["target", "node_modules", "dist", ".git", "gen"]);

const RQ = /RQ-[A-Z]+-\d{3}/g;

/** ANSI helpers that degrade to nothing when output is piped. */
const tty = process.stdout.isTTY;
const red = (s) => (tty ? `[31m${s}[0m` : s);
const green = (s) => (tty ? `[32m${s}[0m` : s);
const bold = (s) => (tty ? `[1m${s}[0m` : s);
const dim = (s) => (tty ? `[2m${s}[0m` : s);

function walk(dir, out = []) {
  if (!existsSync(dir)) return out;
  for (const entry of readdirSync(dir)) {
    if (SKIP_DIRS.has(entry)) continue;
    const full = join(dir, entry);
    const info = statSync(full);
    if (info.isDirectory()) walk(full, out);
    else if (SOURCE_EXTENSIONS.some((ext) => entry.endsWith(ext))) out.push(full);
  }
  return out;
}

/** Requirements declared in the spec tables: rows whose first cell is a backticked RQ id. */
function collectRequirements() {
  const requirements = new Map(); // id -> { spec, line, text }
  const duplicates = [];

  for (const file of readdirSync(SPEC_DIR).filter((f) => /^SPEC-\d+.*\.md$/.test(f)).sort()) {
    const path = join(SPEC_DIR, file);
    const lines = readFileSync(path, "utf8").split("\n");
    lines.forEach((line, index) => {
      const match = /^\|\s*`(RQ-[A-Z]+-\d{3})`\s*\|\s*(.+?)\s*\|/.exec(line);
      if (!match) return;
      const [, id, text] = match;
      if (requirements.has(id)) duplicates.push(id);
      requirements.set(id, { spec: file, line: index + 1, text });
    });
  }
  return { requirements, duplicates };
}

/** `Covers:` annotations found anywhere in the tracked sources. */
function collectCoverage() {
  const coverage = new Map(); // id -> [locations]
  for (const root of SOURCE_ROOTS) {
    for (const file of walk(join(ROOT, root))) {
      // The spec files declare requirements; they do not cover them.
      if (file.startsWith(SPEC_DIR)) continue;
      const text = readFileSync(file, "utf8");
      if (!text.includes("Covers:")) continue;

      text.split("\n").forEach((line, index) => {
        const marker = line.indexOf("Covers:");
        if (marker === -1) return;
        const ids = line.slice(marker).match(RQ) ?? [];
        for (const id of ids) {
          const where = `${relative(ROOT, file).replaceAll("\\", "/")}:${index + 1}`;
          coverage.set(id, [...(coverage.get(id) ?? []), where]);
        }
      });
    }
  }
  return coverage;
}

/**
 * Structural claims the specs make about the repository itself.
 *
 * These are requirements no unit test can express, because they are about the shape of the
 * repository rather than the behaviour of a function: a profile setting, a lint level, a CSP
 * string, the way actions are pinned. Checking them here is what stops them from being
 * promises in a document that nobody verifies.
 *
 * Covers: RQ-CORE-004, RQ-CORE-005, RQ-SEC-010, RQ-SEC-013, RQ-SEC-015, RQ-UI-004, RQ-UI-010
 */
function structuralChecks() {
  const failures = [];
  const read = (rel) => {
    const path = join(ROOT, rel);
    return existsSync(path) ? readFileSync(path, "utf8") : null;
  };

  // RQ-CORE-005: a panic must never unwind out of a real-time audio callback.
  const manifest = read("Cargo.toml") ?? "";
  if (!/\[profile\.release\][\s\S]*?panic\s*=\s*"abort"/.test(manifest)) {
    failures.push('RQ-CORE-005: [profile.release] does not set panic = "abort"');
  }

  // RQ-SEC-010 / RQ-CORE-004: unsafe is denied workspace-wide.
  if (!/unsafe_code\s*=\s*"deny"/.test(manifest)) {
    failures.push('RQ-SEC-010: workspace lints do not set unsafe_code = "deny"');
  }

  // RQ-UI-004 / RQ-UI-010 / RQ-SEC-013: the WebView runs under a strict CSP.
  const tauriConf = read("apps/desktop/src-tauri/tauri.conf.json");
  if (tauriConf) {
    const conf = JSON.parse(tauriConf);
    const csp = conf?.app?.security?.csp ?? "";
    if (!csp) failures.push("RQ-UI-004: tauri.conf.json declares no CSP");

    for (const forbidden of ["unsafe-inline", "unsafe-eval", "unsafe-hashes", "*"]) {
      if (csp.includes(forbidden)) {
        failures.push(`RQ-UI-004: CSP contains "${forbidden}"`);
      }
    }

    // Tauri routes its own IPC over `http://ipc.localhost` on Windows, so the requirement is
    // "no *remote* origin", not "no http". Anything that is not one of Tauri's own loopback
    // origins is a remote origin as far as this WebView is concerned.
    const TAURI_IPC_ORIGINS = ["http://ipc.localhost", "http://tauri.localhost"];
    const origins = csp.match(/https?:\/\/[^\s;]+/g) ?? [];
    for (const origin of origins) {
      if (!TAURI_IPC_ORIGINS.includes(origin)) {
        failures.push(`RQ-UI-004: CSP allows the remote origin "${origin}"`);
      }
    }
    if (conf?.app?.withGlobalTauri !== false) {
      failures.push("RQ-UI-010: withGlobalTauri must be false");
    }
  }

  // RQ-SEC-015: actions pinned by SHA, not by a mutable tag.
  const workflowDir = join(ROOT, ".github", "workflows");
  if (existsSync(workflowDir)) {
    for (const file of readdirSync(workflowDir).filter((f) => f.endsWith(".yml"))) {
      const text = readFileSync(join(workflowDir, file), "utf8");
      text.split("\n").forEach((line, index) => {
        const uses = /^\s*-?\s*uses:\s*([^\s#]+)/.exec(line);
        if (!uses) return;
        const ref = uses[1];
        if (ref.startsWith("./")) return; // a local composite action needs no pin
        if (!/@[0-9a-f]{40}$/.test(ref)) {
          failures.push(
            `RQ-SEC-015: .github/workflows/${file}:${index + 1} uses "${ref}", not a 40-char commit SHA`,
          );
        }
      });
    }
  }

  return failures;
}

/** Rewrites each spec's "Covered by" column from the annotations that exist. */
function syncTables(coverage) {
  let changed = 0;
  for (const file of readdirSync(SPEC_DIR).filter((f) => /^SPEC-\d+.*\.md$/.test(f))) {
    const path = join(SPEC_DIR, file);
    const original = readFileSync(path, "utf8");
    const updated = original
      .split("\n")
      .map((line) => {
        const match = /^(\|\s*`(RQ-[A-Z]+-\d{3})`\s*\|[^|]*\|[^|]*\|)([^|]*)\|(.*)$/.exec(line);
        if (!match) return line;
        const [, head, id, , tail] = match;
        const locations = coverage.get(id) ?? [];
        const cell = locations.length ? locations.map((l) => `\`${l}\``).join("<br>") : "—";
        return `${head} ${cell} |${tail}`;
      })
      .join("\n");
    if (updated !== original) {
      writeFileSync(path, updated, "utf8");
      changed += 1;
    }
  }
  return changed;
}

function main() {
  const { requirements, duplicates } = collectRequirements();
  const coverage = collectCoverage();

  if (requirements.size === 0) {
    console.error(red("spec-guard: no requirements found. Is specs/ present?"));
    process.exit(2);
  }

  const uncovered = [...requirements.keys()].filter((id) => !coverage.has(id)).sort();
  const unknown = [...coverage.keys()].filter((id) => !requirements.has(id)).sort();
  const structural = structuralChecks();

  console.log(bold("spec-guard"));
  console.log(`  requirements declared : ${requirements.size}`);
  console.log(`  requirements covered  : ${requirements.size - uncovered.length}`);
  console.log(`  coverage annotations  : ${[...coverage.values()].flat().length}`);

  if (WRITE) {
    const changed = syncTables(coverage);
    console.log(`  spec tables rewritten : ${changed}`);
  }

  let failed = false;

  if (duplicates.length) {
    failed = true;
    console.error(red(`\n  duplicate requirement ids (ids are append-only and unique):`));
    for (const id of [...new Set(duplicates)].sort()) console.error(`    ${id}`);
  }

  if (uncovered.length) {
    failed = true;
    console.error(red(`\n  ${uncovered.length} requirement(s) with no test:`));
    for (const id of uncovered) {
      const info = requirements.get(id);
      console.error(`    ${id}  ${dim(`${info.spec}:${info.line}`)}`);
      console.error(`      ${dim(info.text.slice(0, 96))}`);
    }
    console.error(
      dim("\n    Add `/// Covers: <id>` above the test that proves it, or withdraw the requirement."),
    );
  }

  if (unknown.length) {
    failed = true;
    console.error(red(`\n  ${unknown.length} annotation(s) naming a requirement that does not exist:`));
    for (const id of unknown) {
      console.error(`    ${id}  ${dim((coverage.get(id) ?? []).join(", "))}`);
    }
  }

  if (structural.length) {
    failed = true;
    console.error(red(`\n  ${structural.length} structural check(s) failed:`));
    for (const failure of structural) console.error(`    ${failure}`);
  }

  if (failed) {
    console.error(red("\nspec-guard: FAILED"));
    process.exit(1);
  }
  console.log(green("\nspec-guard: every requirement is covered."));
}

main();
