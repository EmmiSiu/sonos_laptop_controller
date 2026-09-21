/**
 * Display-side hardening for strings that came from the network.
 *
 * Room and model names are supplied by whatever answered an SSDP probe. The Rust side already
 * sanitises and truncates them (`DeviceName`), and Vue escapes interpolated text, and the CSP
 * forbids inline script. This is the fourth layer, and it exists because the first three are
 * each one refactor away from being removed by someone who does not know why they are there.
 *
 * Nothing here is a substitute for those layers. It is a belt on top of three braces.
 */

/** Longest string the interface will render for a device field. */
export const MAX_DISPLAY_LENGTH = 64;

/**
 * Makes an untrusted string safe to render as text.
 *
 * Strips control characters, collapses whitespace, and truncates on a code-point boundary so a
 * crafted name cannot fake interface structure or push the layout off screen.
 */
export function displayText(raw: unknown, max: number = MAX_DISPLAY_LENGTH): string {
  if (typeof raw !== "string") {
    return "";
  }

  // eslint-disable-next-line no-control-regex -- stripping control characters is the point
  const stripped = raw.replace(/[\u0000-\u001f\u007f-\u009f]/g, " ");
  const collapsed = stripped.split(/\s+/).filter(Boolean).join(" ");

  // `Array.from` splits by code point, so a surrogate pair is never cut in half.
  const points = Array.from(collapsed);
  if (points.length <= max) {
    return collapsed;
  }
  return points.slice(0, Math.max(0, max - 1)).join("") + "\u2026";
}

/** Formats a frame count as `H:MM:SS`, for the session timer. */
export function formatElapsed(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) {
    return "0:00";
  }
  const total = Math.floor(seconds);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const secs = total % 60;
  const mm = String(minutes).padStart(hours > 0 ? 2 : 1, "0");
  const ss = String(secs).padStart(2, "0");
  return hours > 0 ? `${hours}:${mm}:${ss}` : `${mm}:${ss}`;
}
