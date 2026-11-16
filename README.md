# Grok Home Voice Assistant (Rust)

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/rust-1.80+-orange.svg)](https://www.rust-lang.org/)
[![Docker](https://img.shields.io/badge/docker-ready-blue)](https://www.docker.com/)

**A high-performance, low-latency open-source home voice assistant written in Rust, powered by xAI's Grok Voice Agent Realtime API.**

Designed for snappy, real-time conversational control of your smart home. Rust's zero-cost abstractions, async runtime (Tokio), and efficient audio handling deliver sub-second response times with minimal resource usage—ideal for always-on homelab deployment.

## Why Rust?

- **Snappy & Fast**: Minimal latency in audio capture, WebSocket streaming, and playback
- **Efficient**: Low memory and CPU footprint for 24/7 operation
- **Reliable**: Strong type safety and excellent async concurrency with Tokio
- **Production-ready**: Suitable for continuous home deployment

## Features

### Core Voice Pipeline
- Real-time speech-to-speech via xAI Grok Voice Agent API (`wss://api.x.ai/v1/realtime`)
- Server-side Voice Activity Detection (VAD)
- Low-latency PCM audio streaming (24 kHz)
- Multiple high-quality voices (Eve, Ara, Rex, Leo, Sal, and more)
- Efficient WebSocket handling with `tokio-tungstenite`

### Smart Home Integration
- Home Assistant REST API client for entity control
- MQTT support (planned)
- Natural language device commands
- Extensible skill system

### Deployment
- Docker multi-stage build for small images
- Environment-based configuration
- Structured logging
- Graceful shutdown

## Quick Start

### Prerequisites
- Rust 1.80+ (or use Docker)
- xAI API key with Voice access ([console.x.ai](https://console.x.ai))
- Microphone and speakers (for local audio)

### Local Build & Run

```bash
git clone https://github.com/Ajsparty/grok-home-assistant.git
cd grok-home-assistant
cp .env.example .env
# Edit .env with your XAI_API_KEY
cargo run --release
```

### Docker

```bash
docker compose up -d --build
```

## Configuration

Copy `.env.example` to `.env`:

```env
XAI_API_KEY=your_key_here
VOICE=eve
HA_URL=http://homeassistant.local:8123
HA_TOKEN=your_long_lived_token
```

## Architecture

The application uses Tokio for async concurrency:

1. Optional local wake-word / continuous listening mode
2. WebSocket connection to Grok Realtime API
3. Bidirectional audio streaming (PCM16 @ 24 kHz)
4. Tool calling for Home Assistant / external services
5. Efficient audio playback

## Roadmap

- [ ] Local wake-word detection (via ONNX Runtime or FFI to openWakeWord)
- [ ] Full duplex audio with echo cancellation
- [ ] MQTT skill
- [ ] Timer / reminder skill
- [ ] Web status dashboard
- [ ] Multi-room satellite support
- [ ] Custom voice cloning

## Contributing

Contributions welcome. Please open an issue first for major changes.

## License

MIT License — see [LICENSE](LICENSE).

## Disclaimer

Independent open-source project. Not affiliated with xAI. Audio after activation is processed by xAI's cloud APIs. Review their privacy policy.
