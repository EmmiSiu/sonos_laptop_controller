# Latency

The most common question about Rincon, answered before anyone has to ask it.

## The short answer

**Audio arrives at the speaker 1–2 seconds late, and nothing Rincon does can change that.**

Rincon's own contribution to the delay is under 20 milliseconds. The rest is a buffer inside
the Sonos firmware, and there is no protocol for asking it to be smaller.

| Use case | Verdict |
| -------- | ------- |
| Music, podcasts, radio, anything you only listen to | Perfect |
| Video where you also watch the screen | Noticeably out of sync |
| Games, calls, live monitoring | Not usable |

## Where the time goes

```mermaid
flowchart LR
    A["App makes sound"] -->|"~0 ms"| B["OS mixer"]
    B -->|"3–10 ms<br/>WASAPI period"| C["Rincon captures"]
    C -->|"< 1 ms<br/>lock-free ring"| D["Rincon serves"]
    D -->|"1–5 ms<br/>LAN hop"| E["Speaker receives"]
    E -->|"<b>1000–2000 ms</b><br/>firmware jitter buffer"| F["🔊 Sound"]

    style E fill:#7f1d1d,stroke:#dc2626,color:#fee2e2
    style F fill:#065f46,stroke:#10b981,color:#ecfdf5
```

Everything before the red box is ours, and it totals **under 20 ms** — less than the delay from
sitting three metres further from a speaker. The red box is 98% of the latency.

## Why the speaker buffers

Sonos players are designed for multi-room playback over Wi-Fi, which imposes two constraints
Rincon cannot argue with:

1. **Wi-Fi is bursty.** A microwave, a neighbour's access point, or someone walking between the
   router and the speaker produces gaps of tens to hundreds of milliseconds. A speaker with a
   small buffer stutters; one with a second of audio in hand does not.
2. **Rooms must stay in sync.** Two speakers playing the same track have to agree on when each
   sample plays, to within a few milliseconds, or the result is unlistenable. That agreement
   needs slack, and slack is buffer.

Both are the right decisions for the product Sonos built. They are simply expensive for ours.

## What we can and cannot do

| | Effect |
| --- | --- |
| Reduce our own buffering | Already under 20 ms. Halving it saves 10 ms of 1500 |
| Send raw PCM instead of an encoded format | Already do. Saves the encoder's few ms |
| Ask the speaker to buffer less | No such command exists in the UPnP surface Sonos exposes |
| Use the Sonos low-latency TV path | Only for a Beam or Arc over HDMI-ARC, from a TV, not from a laptop |
| Delay the video instead | Possible in a media player, not something Rincon can do for the whole system |

**If you need lip sync with video**, a Bluetooth or an AirPlay 2 speaker is the right tool.
AirPlay 2 typically lands around 200 ms, which is still visible but far less so.

## Measuring it yourself

```bash
cargo run -p rincon-cli -- stream
```

Open the printed URL in VLC on the same machine. VLC's delay is a few tens of milliseconds, so
what you hear there is roughly Rincon's own contribution — confirming that the rest is the
speaker.

For the full figure: clap sharply next to the laptop with the session running, and count how
long until it comes out of the speaker. Record the result in
[`manual-test-log.md`](manual-test-log.md).

## What the interface says

The connected state carries the sentence:

> Sonos speakers buffer 1-2 seconds. Great for music; video will look out of sync.

It is on screen, not in a menu, and `RQ-UI-006` exists so that it stays there. A user who finds
out about the delay by starting a film concludes the app is broken — and from where they are
standing, they are right.
