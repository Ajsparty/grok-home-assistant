# Configuration

Values come from the environment. `cargo run` loads a `.env` file in the working directory. Docker Compose loads `.env` and overrides `AUDIO_OUTPUT` to `satellite`.

The status console can change the voice-agent settings and writes them to `data/dashboard.json`. That file wins over the environment the next time the process starts. API keys, Home Assistant tokens, and the dashboard password are not in it.

Placeholder values that contain `your_` are treated as unset for Home Assistant, MCP, collection settings, and `DASHBOARD_PASSWORD`. `XAI_API_KEY` is required and is not filtered.

## Voice

| Variable | Default | Purpose |
| --- | --- | --- |
| `XAI_API_KEY` | | Bearer token for the realtime socket and vision calls |
| `VOICE` | `eve` | Built-in voice name or a custom voice id |
| `VOICE_MODEL` | `grok-voice-latest` | Realtime model query parameter |
| `XAI_VISION_MODEL` | `grok-4` | Model for doorbell and `describe_camera` snapshots |
| `LANGUAGE_HINT` | | Optional BCP-47 transcription hint, such as `es-MX` |
| `ENABLE_WEB_SEARCH` | `true` | Register the server-side `web_search` tool |
| `ENABLE_X_SEARCH` | `false` | Register the server-side `x_search` tool |
| `COLLECTION_ID` | | Register `file_search` for that xAI collection |
| `ROOM` | `living room` | Room name in the instructions, keyterms, and "the lights" |

## Wake and session

| Variable | Default | Purpose |
| --- | --- | --- |
| `WAKE_MODE` | `energy` | `energy` or `always` |
| `WAKE_PHRASE` | `hey grok` | Required in the user transcript before tools run. Empty disables the check |
| `WAKE_RMS` | `0.05` | Microphone level that counts as activity. Raise it if the TV holds the session open |
| `WAKE_HOLD_MS` | `250` | How long the level must stay high before the socket opens |
| `SILENCE_SECS` | `8` | Quiet time before an energy session closes |
| `FOLLOW_UP_SECS` | `20` | How long tools stay allowed after the wake phrase |
| `SESSION_ROTATE_MINS` | `110` | Reconnect before the 120 minute API cap. The conversation id is kept for 25 minutes |

`WAKE_RMS` is also the level that counts as "someone is still here" for the silence timer. Set it above the room's noise floor.

## Audio

| Variable | Default | Purpose |
| --- | --- | --- |
| `AUDIO_OUTPUT` | `auto` | `auto`, `local`, `satellite`, or `both` |
| `ECHO_DELAY_MS` | `80` | How far behind the speaker is, for the subtract |
| `ECHO_GAIN` | `0.3` | Subtract gain. `0` disables it |
| `BARGE_IN` | `true` | Stop playback when the mic is loud during speech |
| `BARGE_RMS` | `0.12` | Level that counts as barge-in |

`auto` sends the reply to the satellite that most recently crossed the wake level, or to the local speaker when the local mic did. While assistant audio is playing, mic audio is not uploaded until barge-in. The echo subtract is used to decide that barge-in, and the subtracted samples are what get uploaded after it.

The local device is opened only when the crate is built with the `local-audio` feature (the default). The Docker image uses `--no-default-features`.

## Home Assistant

| Variable | Default | Purpose |
| --- | --- | --- |
| `HA_URL` | | Base URL, for example `http://homeassistant.local:8123` |
| `HA_TOKEN` | | Long-lived access token |
| `WEATHER_ENTITY` | first `weather.*` | Morning brief |
| `CALENDAR_ENTITY` | first `calendar.*` | Next 18 hours in the morning brief |
| `MEDIA_ENTITY` | playing player, else the first | Fast path and `media_control` |
| `TODO_ENTITY` | | Default list for `todo_list` |
| `DOORBELL_CAMERA` | camera id containing `doorbell` or `front` | Snapshot source |
| `DOORBELL_ENTITIES` | | Extra entity ids treated as doorbells |
| `ALERT_ENTITIES` | | Extra entity ids to watch |
| `ALERT_MATCH` | `doorbell,leak,smoke,carbon_monoxide,moisture,water_leak` | Substrings that mark a watched entity |
| `CRITICAL_MATCH` | `smoke,carbon_monoxide,leak,moisture,gas` | These speak even in quiet hours |
| `ALERT_POLL_SECS` | `5` | State poll interval |
| `CONFIRM_DOMAINS` | `lock,alarm_control_panel` | Services in these domains wait for confirmation |
| `CONFIRM_MATCH` | `garage_door,front_door,unlock,disarm` | Match against `service` and `entity_id` |
| `HA_MCP_URL` | | Remote MCP server registered on the voice session |
| `HA_MCP_TOKEN` | | Sent to xAI as the MCP `authorization` value |
| `HA_MCP_ALLOWED_TOOLS` | | Required. The MCP tool is omitted when this list is empty |
| `ANNOUNCE_WHEN_EMPTY` | `false` | Speak non-critical alerts when every person is away |
| `QUIET_HOURS` | `true` | Enable the quiet window |
| `QUIET_START` | `22:00` | Local time, `HH:MM` |
| `QUIET_END` | `07:00` | Local time. An overnight window is supported |

Allowed `call_service` domains: `light`, `switch`, `fan`, `climate`, `cover`, `media_player`, `todo`, `scene`, `script`, `input_boolean`, `input_number`, `vacuum`, `humidifier`, `remote`, `button`, `number`, `select`, `lock`, `alarm_control_panel`.

Fast-path "turn on/off" only targets `light`, `switch`, `fan`, `input_boolean`, `humidifier`, and `media_player`. "The lights" means lights whose entity id or friendly name contains `ROOM`.

Aliases live in `ALIASES_FILE` (default `aliases.json`):

```json
{ "big light": "light.living_room" }
```

Keys are matched after lowercasing and stripping punctuation. Copy `aliases.example.json` to start.

## Process

| Variable | Default | Purpose |
| --- | --- | --- |
| `HEALTH_BIND` | `0.0.0.0` | Status page bind host. `HEALTH_PORT=0` disables it |
| `HEALTH_PORT` | `8080` | `GET /` is the page, `GET /health` is JSON |
| `SATELLITE_BIND` | `0.0.0.0` | Satellite WebSocket host. `SATELLITE_PORT=0` disables it |
| `SATELLITE_PORT` | `8765` | Satellite port |
| `LOG_DIR` | `logs` | `audit.jsonl` is appended here |
| `DATA_DIR` | `data` | `timers.json`, `alarms.json`, `conversation.json`, `dashboard.json`, `console-lock.json`, and `last-doorbell.jpg` |
| `DASHBOARD_PASSWORD` | | Optional console password, at least 8 characters. Empty leaves the status page open. A value containing `your_` is ignored. The environment password wins over the file on startup |
| `ALIASES_FILE` | `aliases.json` | Nickname file |
| `RUST_LOG` | | `info` is a good start |

## Satellite protocol

Connect to `ws://<hub>:<SATELLITE_PORT>`.

1. Send a text frame: `{"type":"hello","id":"kitchen","room":"kitchen"}`.
2. Send binary frames of PCM16 little-endian, mono, 24 kHz. Frames larger than two seconds are dropped.
3. Receive binary frames of the same format, and `{"type":"clear"}` when playback should stop.
4. `{"type":"wake"}` opens a voice session even if the level is low.
5. `{"type":"ping"}` is answered with `{"type":"pong"}`.

`grok-satellite` does this with the default microphone and speaker. It reads `HUB_URL` (default `ws://127.0.0.1:8765`), `SATELLITE_ID`, and `ROOM`.

## Status JSON

`GET /health` includes connection state, room, active source, mic level, people home, quiet hours, the last tool, the last transcript, the voice note, wake-phrase state, satellites, doorbell caption, pending confirmation, timers, clock alarms, and recent events. It does not include the API key, the Home Assistant token, or the dashboard password.

The console pages are `/live`, `/activity`, `/house`, `/music`, `/alarms`, `/timers`, and `/settings`. `GET /api/settings` and `PUT /api/settings` read and replace the voice preferences. Music, lights, and alarm panels go through the same Home Assistant allowlist as voice. Arming or disarming a panel stages the same 45-second confirmation as a spoken lock command. The alarm code is sent only with that pending action.

Clock alarms in `data/alarms.json` fire on the local clock and speak even during quiet hours. Music uses `media_player` services, including `select_source` and `play_media`. A player is badged Apple Music or Music Assistant when its Home Assistant source or app name says so. There is no Apple account login.

If `DASHBOARD_PASSWORD` or `data/console-lock.json` is set, `/health` and `/api` (except login and lock status) require the `grok_console` cookie. The HTML pages stay reachable so the sign-in form can render. Sessions live in memory and end when the process restarts.
