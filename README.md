# Grok Home Voice Assistant (Rust)

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/rust-1.80+-orange.svg)](https://www.rust-lang.org/)
[![Docker](https://img.shields.io/badge/docker-ready-blue)](https://www.docker.com/)

A home voice assistant written in Rust and powered by xAI's Grok Voice Agent realtime API. It stays quiet until the room is active, then streams 24 kHz PCM, calls a small set of Home Assistant tools, and hangs up after silence.

Audio after the session opens is processed by xAI. The socket stays closed until local wake, and it is rotated before the API's 120 minute session cap.

## Features

### Voice session

- Authenticated WebSocket to `wss://api.x.ai/v1/realtime` (`Authorization: Bearer`)
- Server VAD, PCM16 at 24 kHz, and any built-in or custom voice id (`VOICE`)
- Session resumption across reconnects, with rotation at 110 minutes
- Reconnect with backoff after a dropped session
- Web search by default. X search, a household collection (`file_search`), and a read-only MCP server are optional
- Local microphone and speaker through `cpal`, with linear resampling to the device rate
- Barge-in stops playback when the mic stays loud during speech
- Fixed-delay echo subtract (`ECHO_DELAY_MS`, `ECHO_GAIN`) so the mic can be compared with what was just played
- Earcons for a local acknowledgement, a timer, and an alert

### Wake and cost

- `WAKE_MODE=energy` (default) opens the socket only after the microphone stays above `WAKE_RMS`
- The socket closes again after `SILENCE_SECS` of a quiet room
- `WAKE_PHRASE` (default `hey grok`) must be in the transcript before device tools or the fast path run. A follow-up window covers the next sentence
- `WAKE_MODE=always` keeps one session open until rotation. Set `WAKE_PHRASE=` to skip the phrase check
- System announcements (timers you asked for, critical alerts) arm the phrase gate themselves

This is an energy gate plus a transcript phrase. A dedicated on-device model such as openWakeWord is not bundled.

### Home Assistant

Tools registered on the voice session:

| Tool | What it does |
| --- | --- |
| `search_entities` | Resolve a nickname or room to entity ids. Aliases in `aliases.json` win |
| `get_state` | Read one entity, with a short attribute list |
| `call_service` | Allowlisted domains only |
| `confirm_action` | Runs a pending lock, alarm, or garage action after a clear yes |
| `media_control` | Play, pause, skip, and volume |
| `todo_list` | List, add, or complete a to-do item |
| `set_timer`, `cancel_timer`, `list_timers` | Local timers that survive the voice socket |
| `morning_brief` | Weather, calendar, presence, recent events, timers |
| `who_is_home` | `person` entities that are `home` |
| `describe_camera` | Snapshot plus a short vision description |
| `recent_events` | Doorbell lines, alerts, and timers kept in memory |

`call_service` refuses domains outside the allowlist. Locks, alarm panels, and entity ids matching `CONFIRM_MATCH` (garage doors, front door) return `needs_confirmation` and wait 45 seconds for `confirm_action`.

Plain commands are also handled locally when the phrase gate is open, so "turn off the office lamp", "pause", and "set a timer for 5 minutes" do not wait on a second model round trip. The same action is ignored if the model repeats it within 8 seconds. Locks and garage commands stay on the confirmation path.

`ROOM` is the room for this box. "Turn off the lights" targets `light` entities whose names include that room.

### House events

- Home Assistant is polled for rising alerts (`on`, `open`, leak, smoke, and the `ALERT_MATCH` list)
- A doorbell event takes `DOORBELL_CAMERA` (or a camera whose id contains `doorbell` or `front`), saves the frame, asks the vision model for one sentence, and speaks it
- Critical matches (smoke, leak, gas, and `CRITICAL_MATCH`) speak during quiet hours and when the house is empty
- Clock alarms on the Alarms page also speak during quiet hours. Countdown timers still chime, and their speech follows quiet hours and presence
- Other alerts stay in the recent-events list during quiet hours, and when every `person` entity is away
- If you have no `person` entities, the house is treated as occupied
- Quiet hours default to 22:00–07:00

### Rooms, status, and shutdown

- Satellites connect to `ws://hub:8765`, send PCM16 mono at 24 kHz, and receive playback. See `grok-satellite`
- `AUDIO_OUTPUT=auto` plays the reply in the room that just spoke
- A status console on port 8080 with Live, Activity, House, Music, Alarms, Timers, and Settings pages. `GET /health` is the JSON feed
- Music controls a Home Assistant `media_player`. Apple Music works when that player is a HomePod, Apple TV, or Music Assistant source. The console does not sign in to Apple
- Lights in the House page call `light.turn_on` and `light.turn_off`. Locks, alarm panels, and garage actions still wait for Yes
- `DASHBOARD_PASSWORD`, or a password set in Settings, puts an HttpOnly cookie in front of `/health` and `/api`. Leave it unset and the console stays open
- Tool calls append a JSON line to `logs/audit.jsonl`
- Ctrl+C, and SIGTERM on Unix, stop the tasks

## Quick start

### Prerequisites

- Rust 1.80+ (or Docker)
- An xAI API key with voice access ([console.x.ai](https://console.x.ai))
- A microphone and speaker for the local build
- Home Assistant long-lived token, if you want device control

### Local build

```bash
git clone https://github.com/Ajsparty/grok-home-assistant.git
cd grok-home-assistant
cp .env.example .env
cp aliases.example.json aliases.json
# Edit .env and aliases.json
cargo run --release
```

The status page is at [http://127.0.0.1:8080](http://127.0.0.1:8080).

A second machine in another room sets `HUB_URL`, `SATELLITE_ID`, and `ROOM`, then runs the satellite binary. That binary is built with the default `local-audio` feature.

```bash
HUB_URL=ws://192.168.1.20:8765 SATELLITE_ID=kitchen ROOM=kitchen cargo run --release --bin grok-satellite
```

```powershell
$env:HUB_URL = "ws://192.168.1.20:8765"
$env:SATELLITE_ID = "kitchen"
$env:ROOM = "kitchen"
cargo run --release --bin grok-satellite
```

### Docker

The image is built without `cpal`. Run `grok-satellite` on machines that have microphones, and point them at the hub.

```bash
docker compose up -d --build
```

Compose publishes `8080` (status) and `8765` (satellites) and sets `AUDIO_OUTPUT=satellite`. If `logs` or `data` are not writable by the container user, fix the directory permissions on the host.

## Configuration

Copy `.env.example` to `.env`. The full list is in [docs/configuration.md](docs/configuration.md).

```env
XAI_API_KEY=your_key_here
VOICE=eve
ROOM=living room
HA_URL=http://homeassistant.local:8123
HA_TOKEN=your_long_lived_token
WAKE_PHRASE=hey grok
```

`aliases.json` maps spoken names to entity ids:

```json
{ "big light": "light.living_room" }
```

## Architecture

Tokio tasks share an `AppState` and an audio hub:

1. Local audio, or a satellite, sends 24 kHz PCM into the hub.
2. The hub measures level, applies the echo subtract while audio is playing, and either wakes the session or forwards the frame.
3. The voice task authenticates, sends `session.update` (instructions, tools, resumption, transcription), and streams `input_audio_buffer.append`.
4. Tool calls run against Home Assistant or the local timer store. Dangerous calls wait for `confirm_action`.
5. Assistant audio is played locally or sent to the active satellite. Barge-in clears that buffer and sends `response.cancel`.
6. Silence, or the rotation timer, closes the socket. The conversation id is reused for 25 minutes.

## Roadmap

- [x] Local wake gate and phrase check
- [x] Barge-in and a fixed-delay echo subtract
- [x] Timer and reminder skill
- [x] Status page
- [x] Multi-room satellite audio
- [x] Custom voice ids (`VOICE` accepts an xAI custom voice id)
- [ ] On-device phrase model (openWakeWord or ONNX) so the wake phrase is recognized before any audio is uploaded
- [ ] Adaptive echo cancellation
- [ ] MQTT skill
- [ ] A guided voice-cloning flow (creating the voice id still happens in the xAI console)

## Contributing

Contributions welcome. Please open an issue first for major changes.

## License

MIT License — see [LICENSE](LICENSE).

## Disclaimer

Independent open-source project. Not affiliated with xAI. After the session opens, audio and tool text are sent to xAI. Camera snapshots used by doorbell and `describe_camera` are sent to the vision model. Review xAI's privacy policy before leaving this running.
