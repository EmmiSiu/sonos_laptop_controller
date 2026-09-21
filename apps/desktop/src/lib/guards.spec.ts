import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * Structural checks on the interface source.
 *
 * These are greps, which is usually a smell. Here it is the right tool: the property being
 * checked is "this construct appears nowhere", and a grep is the only thing that can say that
 * about code nobody has written yet.
 */

const SRC = join(dirname(fileURLToPath(import.meta.url)), "..");

function sources(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) {
      sources(full, out);
    } else if (/\.(ts|vue)$/.test(entry) && !entry.endsWith(".spec.ts")) {
      out.push(full);
    }
  }
  return out;
}

describe("interface hardening", () => {
  // Covers: RQ-UI-005
  it("never uses v-html, innerHTML, or eval", () => {
    // Every string the interface renders may have come from an unauthenticated device on the
    // LAN. Vue escapes interpolated text; `v-html` deliberately does not, which is exactly
    // why it must not appear anywhere in this tree.
    const forbidden = [
      { pattern: /\bv-html\b/, why: "v-html renders untrusted device names as markup" },
      { pattern: /\.innerHTML\s*=/, why: "innerHTML bypasses Vue's escaping" },
      { pattern: /\bouterHTML\s*=/, why: "outerHTML bypasses Vue's escaping" },
      { pattern: /\beval\s*\(/, why: "eval is forbidden by the CSP and by common sense" },
      { pattern: /new\s+Function\s*\(/, why: "Function() is eval with extra steps" },
      { pattern: /\bdangerouslySet/, why: "no framework here should offer this" },
    ];

    const offences: string[] = [];
    for (const file of sources(SRC)) {
      const text = readFileSync(file, "utf8");
      for (const { pattern, why } of forbidden) {
        if (pattern.test(text)) {
          offences.push(`${file}: ${pattern} -- ${why}`);
        }
      }
    }

    expect(offences).toEqual([]);
  });

  // Covers: RQ-UI-004
  it("loads nothing from a remote origin", () => {
    // The CSP forbids remote origins, so a remote font or script would simply fail to load at
    // runtime -- in front of a user, with no error anyone would notice. Catch it here instead.
    const remote = /(src|href)\s*=\s*["']https?:\/\//;
    const offences = sources(SRC)
      .filter((file) => remote.test(readFileSync(file, "utf8")))
      .map((file) => `${file} references a remote origin`);

    expect(offences).toEqual([]);
  });
});
