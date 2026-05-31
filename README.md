# RTL-FM
I recently went on vacation in Mexico and saw so many radio antennas around me, yet no one used real radio. That got me thinking: if every city had its own website that acted like a radio receiver, it would let me and many others listen to foreign radio. That's when I realized I had a $40 RTL-SDR lying around, and I decided to put it to work. I started simple, writing the binary 100% in Rust and exposing a web API, and when I realized the audio quality was better than my old FM radio streaming over the network, I decided to design the web app to feel like a high-quality 60s radio. There was only one caveat: the SDR has a single tuner, so I can't play more than one station at a time. To solve that, I added a voting system so everyone (including complete strangers) can agree on a channel. This project has gone further than I expected, becoming part of my daily routine now that I miss some of my home's radio stations while living abroad. If you have an extra SDR lying around, I highly recommend trying this project. It runs on Ubuntu and you don't need to worry about any drivers or libraries. The RTL-SDR is driven directly over USB via [nusb](https://github.com/nickel-lang/nusb) and all the metadata encoding/decoding such as NRSC-5 and RTS is done purely in Rust. This creates a cool website for you, your friends, and the general public.

## Features

- Drives the RTL-SDR directly over USB with [nusb](https://github.com/nickel-lang/nusb): no `librtlsdr`, no kernel driver, no `rtl_tcp`
- Scans the 88-108 MHz FM band at startup and discovers stations automatically via energy detection
- Streams audio over HTTP at three quality profiles: 320 kbps AAC (`low`), 16-bit / 44.1 kHz WAV (`medium`), or 24-bit / 192 kHz WAV (`high`)
- Serves a 60s-radio web UI at `/` showing all scanned stations with live RDS and HD Radio metadata
- Decodes HD Radio (NRSC-5) entirely in Rust under `src/nrsc5/`: station names and album art from the digital sideband
- Collaborative station voting: because the SDR has a single tuner, listeners vote on the one station that plays, and the winner is recomputed live from per-browser heartbeats
- Two roles per server: an anonymous **public** audience (vote + listen to the winner) and a single **admin** operator (full debug/metadata API + arbitrary-station audio), gated by a random token minted at startup
- Idle metadata refresher keeps RDS info current even when nobody is streaming
- `hd` subcommand for direct HD Radio capture to a single station
- `replay` subcommand for deterministic offline decode of recorded IQ captures

## Requirements

- Linux (x86_64)
- An RTL-SDR dongle (RTL2832U-based)
- udev rule installed (see below)
- `ffmpeg` on `PATH`, only for the `low`/AAC profile, which pipes PCM through it. The `medium`/`high` WAV profiles are encoded purely in Rust and need nothing extra.
- Rust toolchain (stable, 2021 edition), only if you build from source

## Install

Prebuilt x86_64 Linux binaries are published on the [Releases](../../releases) page, so you do not need to compile anything. Each release is built automatically by GitHub Actions from the tagged source. Grab the latest:

```sh
# Replace v0.1.0 with the latest tag from the Releases page.
curl -L -o rtl-fm.tar.gz \
  https://github.com/OWNER/REPO/releases/download/v0.1.0/rtl-fm-v0.1.0-x86_64-linux.tar.gz
tar -xzf rtl-fm.tar.gz
chmod +x rtl-fm
sudo mv rtl-fm /usr/local/bin/
```

Each release also ships the `99-rtl-sdr.rules` udev file alongside the binary; install it as described under [udev rule](#udev-rule).

## Build from source

If you prefer to compile it yourself (or are on a non-x86_64 host):

```sh
cargo build --release
# binary at target/release/rtl-fm
```

### Cutting a release

Releases are produced by `.github/workflows/release.yml`, which triggers on any pushed `v*` tag: it builds the binary, runs the tests, and publishes a GitHub Release with the packaged binary and udev rule attached.

```sh
git tag v0.1.0
git push origin v0.1.0
```

## udev rule

The binary accesses the RTL-SDR without root by way of a udev rule. Install it once:

```sh
sudo cp udev/99-rtl-sdr.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger
```

Re-plug the dongle after installing the rule.

## Usage

### Serve (default)

Scan the FM band and start the streaming HTTP server:

```sh
rtl-fm
# or explicitly:
rtl-fm serve
```

Options:

| Flag | Default | Description |
|---|---|---|
| `--listen` | `0.0.0.0:8080` | HTTP listen address |
| `--scan-start-mhz` | `88` | Low edge of scan band |
| `--scan-end-mhz` | `108` | High edge of scan band |
| `--owner` | `""` | Operator name shown under the brand and at `GET /meta` |
| `--location` | `""` | Physical location (e.g. `"San Antonio, TX"`) shown under the brand and at `GET /meta` |
| `--debug` | off | Verbose DSP/decoder logging. Without it the console shows only the scan progress bar, the ready banner, and a live listener count. |

On startup the server prints two URLs:

```
  RTL FM v0.1.0 ready
    Public URL: http://0.0.0.0:8080/
    Admin URL:  http://0.0.0.0:8080/?token=<random-token>
```

Share the **Public URL** with your audience; they can vote and listen to the winning station, nothing else. Open the **Admin URL** yourself: the token is stored as an HttpOnly cookie and unlocks rescans, the full metadata/debug API, and direct per-station audio. The token is regenerated on every run, so keep the admin link private.

### HD Radio capture

Tune directly to one HD station and decode metadata + album art:

```sh
rtl-fm hd --freq 100300000            # 100.3 MHz, 90 s default
rtl-fm hd --freq 100300000 --seconds 120 --record capture.cu8
```

`--record` tees raw IQ to disk so the capture can be re-decoded offline via `replay`.

### Replay

Decode a previously recorded CU8 IQ file without hardware:

```sh
rtl-fm replay --file capture.cu8 --freq 100300000
```

The file must have been captured at 1 488 375 S/s (the rate used by `hd --record`). Decoded album art is written to `album_art_<freq>.<ext>` in the current directory.

## HTTP API

Audio profiles are selected with `?profile=low|medium|high`. A `.aac` path only serves `low`; a `.wav` path serves `medium` (default) or `high`.

### Public (anonymous)

| Method | Path | Description |
|---|---|---|
| `GET` | `/` | Web UI |
| `GET` | `/meta` | Site/operator metadata: version, owner, location, channel count, average signal quality, 24-hour distinct listeners, and active session count |
| `GET` | `/api/whoami` | `{ "admin": bool }`, whether the caller holds the admin token |
| `GET` | `/api/stations` | Scanned stations joined with metadata, listener and vote counts, plus tuner window and current winner |
| `GET` | `/api/stations/stream` | SSE of the station list, pushed once per second |
| `GET` | `/api/vote/state` | Current winner, per-station tallies, and active voter count |
| `POST` | `/api/vote` | Cast/heartbeat a vote, body `{ "fingerprint": str, "freq_hz": u32 }`. Re-sent every few seconds; expires ~12 s after the last heartbeat |
| `POST` | `/api/unvote` | Retract a vote, body `{ "fingerprint": str }` |
| `GET` | `/stream.aac` | 320 kbps AAC of the voted winner. `?fp=<fingerprint>` marks the caller as a live listener |
| `GET` | `/stream.wav` | WAV of the voted winner: `?profile=medium` (16-bit/44.1 kHz, default) or `?profile=high` (24-bit/192 kHz) |

### Admin (requires the startup token via `?token=` or the `rtlfm_admin` cookie)

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/channels` | List scanned channels |
| `GET` | `/api/state` | Tuner and channelizer state (device, center, window, sample rate, active listeners) |
| `GET` | `/api/rescan` | Drop active channels and trigger a fresh band scan |
| `GET` | `/api/metadata` | Cached metadata snapshots for all stations |
| `GET` | `/api/metadata/:khz` | Metadata for one station |
| `GET` | `/api/metadata/:khz/hdscan` | Trigger a 75 s HD Radio scan on one station |
| `GET` | `/api/metadata/:khz/stream` | SSE of one station's metadata, pushed once per second |
| `GET` | `/api/albumart/:khz` | HD Radio album-art image for one station |
| `GET` | `/stream/:file` | Audio for a specific station: `:file` is `<freq>.aac` or `<freq>.wav`, with the same `?profile=` options |

Requests without a valid token receive `401 Unauthorized` on the admin routes.

## Logging

By default the console is quiet (warnings and errors only) so the scan progress bar and live status line are unobstructed; the `hd` and `replay` subcommands stay verbose. Pass `--debug` for the full DSP/decoder trace, or override entirely with `RUST_LOG`:

```sh
RUST_LOG=debug rtl-fm
```

Regardless of console verbosity, all `WARN`/`ERROR` records are also appended to `error.log` in the working directory.
