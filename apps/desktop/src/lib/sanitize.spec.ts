import { describe, expect, it } from "vitest";

import { displayText, formatElapsed, MAX_DISPLAY_LENGTH } from "./sanitize";

describe("displayText", () => {
  // Covers: RQ-UI-011
  it("truncates and strips control characters from device names", () => {
    expect(displayText("Kitchen")).toBe("Kitchen");

    // Control characters cannot be used to fake interface structure.
    expect(displayText("Kitchen\u0000\u0007\n\tSpeaker")).toBe("Kitchen Speaker");
    expect(displayText("Living\r\n\r\nRoom")).toBe("Living Room");

    // Long names are truncated visibly rather than pushing the layout off screen.
    const long = displayText("x".repeat(500));
    expect(Array.from(long)).toHaveLength(MAX_DISPLAY_LENGTH);
    expect(long.endsWith("\u2026")).toBe(true);
  });

  // Covers: RQ-UI-011
  it("never splits a surrogate pair", () => {
    // A byte-index truncation here produces a replacement character, which looks like a bug
    // in the speaker's name rather than in our code.
    const emoji = displayText("\u{1F3B5}".repeat(200));
    expect(Array.from(emoji)).toHaveLength(MAX_DISPLAY_LENGTH);
    expect(emoji).not.toContain("\uFFFD");
  });

  it("returns an empty string for anything that is not a string", () => {
    // The backend is typed, but a renderer that trusts its input completely is a renderer
    // that throws inside a template and shows a blank window.
    for (const value of [null, undefined, 42, {}, [], true]) {
      expect(displayText(value)).toBe("");
    }
  });

  it("leaves markup as text rather than trying to be clever about it", () => {
    // Vue escapes this on render. Stripping it here would silently mangle a legitimate name
    // and hide the fact that escaping is what actually protects us.
    expect(displayText("<b>Kitchen</b>")).toBe("<b>Kitchen</b>");
  });
});

describe("formatElapsed", () => {
  it("formats a session timer the way a clock does", () => {
    expect(formatElapsed(0)).toBe("0:00");
    expect(formatElapsed(9)).toBe("0:09");
    expect(formatElapsed(65)).toBe("1:05");
    expect(formatElapsed(3600)).toBe("1:00:00");
    expect(formatElapsed(3725)).toBe("1:02:05");
  });

  it("does not render nonsense for nonsense input", () => {
    expect(formatElapsed(-1)).toBe("0:00");
    expect(formatElapsed(Number.NaN)).toBe("0:00");
    expect(formatElapsed(Number.POSITIVE_INFINITY)).toBe("0:00");
  });
});
