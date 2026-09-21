# Manual test log

Everything CI cannot do, and a record of who last did it on what.

CI runs the automated suite against fakes, which covers every code path but one class of claim:
*does this actually work on hardware, and does it feel right to a person?* Those checks are
below. Each has an id, so a requirement can point at it, and a date, so "we tested that" has a
shelf life.

**Add a row when you run one.** A checklist with no history is a checklist nobody trusts.

---

## The checklist

### UI-6 — The latency caveat is visible while connected

> Covers: RQ-UI-006

Connect to a speaker and play something. Without opening a menu, scrolling, or hovering
anything, the 1–2 second buffer note must be on screen.

**Why it is manual:** a unit test can assert the string is in the DTO — and one does. It cannot
assert that a human sees it, which is the actual requirement. A note rendered in 8px grey at the
bottom of a scroll container would pass the automated test and fail the intent.

**Pass:** the sentence is legible at arm's length, in the connected state, without interaction.

---

### UI-9 — The window stays interactive during a scan

> Covers: RQ-UI-009

Click **Find my speakers**. While the scan runs (about two seconds):

1. Drag the window. It must move smoothly.
2. Resize it. The layout must reflow, not freeze and snap.
3. Click Cancel. It must respond immediately, not after the scan finishes.

**Why it is manual:** the guarantee is that no command blocks the WebView's main thread.
Everything is `async` and the commands run on Tokio, so this *should* hold by construction —
but "should hold by construction" is exactly the claim that a single accidental blocking call
inside a command quietly breaks, and no test in the suite would notice.

**Pass:** no frame drops, no white window, no "Rincon is not responding".

---

### HW-1 — First sound on real hardware

The whole product, end to end.

1. `cargo run -p rincon-cli -- discover` lists the speaker.
2. Launch the app, scan, connect.
3. Audio plays.
4. Measure the delay: clap near the laptop microphone and count. Record the figure.

**Pass:** audio plays, and the delay is within the 1–2 s the interface claims.

---

### HW-2 — Four-hour session

Start a session and leave it. Check `Dropped` and `Underruns` in the interface at the end.

**Pass:** zero dropped frames (SPEC-000 success metrics). Underruns under 10 are acceptable on
a laptop that also did other work.

---

### HW-3 — Grouped speaker

Group two rooms in the Sonos app, then connect Rincon to the **member**, not the coordinator.

**Pass:** audio plays. A failure here means coordinator resolution is broken, which is
invisible to every test in the suite because the fakes cannot reproduce a real household's
topology document.

---

### HW-4 — Firewall path

On a machine that has never run Rincon, with Windows Firewall at its defaults, connect without
allowing anything first.

**Pass:** either Windows prompts and allowing it works, or the session fails with
`FirewallSuspected` and the panel shows a command that fixes it when run.

---

### HW-5 — Device change mid-session

While streaming, connect a Bluetooth headset so Windows switches the default output.

**Pass:** the session recovers within a few seconds, or fails with a message that says what
happened. It must not hang on a spinner.

---

### HW-6 — No traffic leaves the LAN

Run a packet capture filtered to non-RFC1918 destinations for the duration of a session,
including app launch.

**Pass:** zero packets. This is a product promise (SPEC-000 NG5) and the only way to verify it
end to end is to look.

---

### HW-7 — Idle resource use

With the app open and no session, check Task Manager after five minutes.

**Pass:** under 60 MB RSS, under 2% of one core (SPEC-000 G4).

---

## Log

| Date | Check | Platform | Hardware | Result | Notes |
| ---- | ----- | -------- | -------- | ------ | ----- |
| 2026-09-21 | Current desktop preflight | Windows 11 26200 | No Sonos network available | **pass** | 287 Rust tests and 11 UI tests passed; strict Clippy passed for both workspaces; release `.exe` built; root, Tauri, and npm dependency audits found no unallowlisted vulnerability. A real-network reconfirmation remains intentionally pending. |
| 2026-09-21 | HW-1 (capture half) | Windows 11 26200 | Realtek endpoint | **pass** | WASAPI loopback opened at 48 kHz / 2ch / f32le. 200 KB captured through ring → L16 → WAV → HTTP; 99,978/99,978 samples non-zero, L peak 15 / R peak 68, channels distinct. Header declared `data` = 4294967251, as SPEC-002 specifies. |
| 2026-09-21 | HW-1 (discovery half) | Windows 11 26200 | Sonos "Bedroom", 192.168.0.223 | **pass** | Found on the first scan. Description fetched and parsed, room name correct, `reached_via` correctly chose Wi-Fi over the WSL virtual switch (after the fix below). |
| 2026-09-21 | **HW-1 (playback half)** | Windows 11 26200 | Sonos One "Bedroom" | **pass** | The speaker fetched the stream and **audibly played it**, confirmed by the operator. Full sequence: coordinator → capture → bind → `SetAVTransportURI` → `Play` → peer connected. Verified with both the synthetic 440 Hz tone and the real WASAPI capture. |
| 2026-09-21 | Discovery reliability | Windows 11 26200 | Sonos One "Bedroom" | **pass** | 10/10 scans found the speaker after M-SEARCH retransmission was added. Before it, a single scan intermittently reported "no speakers found" — a lost UDP reply is indistinguishable from an absent device. |
| 2026-09-21 | HW-4 | Windows 11 26200 | — | **partial** | Inbound reached the server, so the path is open — but only because the machine's home Wi-Fi is classified `Public` *and* the rule was added for `Public`. That is the wrong configuration to ship advice for; see the two findings below. |

<!--
Add rows like:
| 2026-09-21 | HW-1 | Windows 11 26200 | Sonos One (S13), fw 70.3 | pass | 1.4 s measured delay |
| 2026-09-21 | HW-3 | Windows 11 26200 | One + Beam grouped | fail | see #42 |
-->
