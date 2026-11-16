# Configuration

## Required

- `XAI_API_KEY`: Your xAI API key with Voice access enabled.

## Optional

- `VOICE`: Default voice (default: `eve`). Options include `eve`, `ara`, `rex`, `leo`, `sal`, and others.
- `HA_URL` / `HA_TOKEN`: Home Assistant integration.
- `RUST_LOG`: Logging level (`info`, `debug`, etc.).

## Audio Notes

The current implementation establishes the Realtime WebSocket and handles response events. Full local microphone capture and speaker playback require additional cpal setup and are planned for the next release. The architecture is designed for minimal latency once audio I/O is wired in.
