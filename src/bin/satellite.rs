use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use futures_util::{SinkExt, StreamExt};
use grok_home_assistant::pcm::{self, SAMPLE_RATE};
use grok_home_assistant::satellite::{clear_message, hello_message};
use std::collections::VecDeque;
use std::env;
use std::sync::{Arc, Mutex};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() {
    env_logger::init();
    if let Err(err) = run().await {
        eprintln!("satellite error: {err:#}");
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    let url = env::var("HUB_URL").unwrap_or_else(|_| "ws://127.0.0.1:8765".to_string());
    let id = env::var("SATELLITE_ID").unwrap_or_else(|_| "satellite".to_string());
    let room = env::var("ROOM").unwrap_or_else(|_| id.clone());
    let (socket, _) = connect_async(&url).await?;
    let (mut write, mut read) = socket.split();
    write
        .send(Message::Text(hello_message(&id, &room).into()))
        .await?;

    let (mic_tx, mut mic_rx) = tokio::sync::mpsc::channel::<(u32, Vec<f32>)>(16);
    let playback: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
    let host = cpal::default_host();
    let _input = host
        .default_input_device()
        .map(|device| open_input(&device, mic_tx))
        .transpose()?;
    let output_rate = host.default_output_device().as_ref().map(|device| {
        device
            .default_output_config()
            .map(|config| config.sample_rate().0)
            .unwrap_or(SAMPLE_RATE)
    });
    let _output = host
        .default_output_device()
        .map(|device| open_output(&device, playback.clone()))
        .transpose()?;
    let output_rate = output_rate.unwrap_or(SAMPLE_RATE);

    log::info!("{id} connected to {url} for {room}");
    loop {
        tokio::select! {
            frame = mic_rx.recv() => {
                let Some((rate, samples)) = frame else { break };
                let pcm = pcm::f32_to_i16(&pcm::resample_linear(&samples, rate, SAMPLE_RATE));
                write.send(Message::Binary(pcm::i16_to_le(&pcm).into())).await?;
            }
            incoming = read.next() => {
                let Some(message) = incoming else { break };
                match message? {
                    Message::Binary(bytes) => {
                        let floats = pcm::i16_to_f32(&pcm::i16_from_le(&bytes));
                        let resampled = pcm::resample_linear(&floats, SAMPLE_RATE, output_rate);
                        if let Ok(mut buffer) = playback.lock() {
                            buffer.extend(resampled);
                        }
                    }
                    Message::Text(text) if clear_message(&text) => {
                        if let Ok(mut buffer) = playback.lock() {
                            buffer.clear();
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn open_input(
    device: &cpal::Device,
    tx: tokio::sync::mpsc::Sender<(u32, Vec<f32>)>,
) -> anyhow::Result<cpal::Stream> {
    let supported = device.default_input_config()?;
    let rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let stream = match format {
        SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _| {
                let _ = tx.try_send((rate, pcm::downmix(data, channels)));
            },
            |err| eprintln!("microphone error: {err}"),
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _| {
                let floats: Vec<f32> = data.iter().map(|sample| *sample as f32 / 32768.0).collect();
                let _ = tx.try_send((rate, pcm::downmix(&floats, channels)));
            },
            |err| eprintln!("microphone error: {err}"),
            None,
        )?,
        SampleFormat::U16 => device.build_input_stream(
            &config,
            move |data: &[u16], _| {
                let floats: Vec<f32> = data.iter().map(|sample| (*sample as f32 / 32768.0) - 1.0).collect();
                let _ = tx.try_send((rate, pcm::downmix(&floats, channels)));
            },
            |err| eprintln!("microphone error: {err}"),
            None,
        )?,
        other => anyhow::bail!("unsupported microphone format {other:?}"),
    };
    stream.play()?;
    Ok(stream)
}

fn open_output(
    device: &cpal::Device,
    buffer: Arc<Mutex<VecDeque<f32>>>,
) -> anyhow::Result<cpal::Stream> {
    let supported = device.default_output_config()?;
    let channels = supported.channels() as usize;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let stream = match format {
        SampleFormat::F32 => device.build_output_stream(
            &config,
            move |data: &mut [f32], _| fill(data, channels, &buffer, |sample| sample),
            |err| eprintln!("speaker error: {err}"),
            None,
        )?,
        SampleFormat::I16 => device.build_output_stream(
            &config,
            move |data: &mut [i16], _| {
                fill(data, channels, &buffer, |sample| (sample.clamp(-1.0, 1.0) * 32767.0) as i16)
            },
            |err| eprintln!("speaker error: {err}"),
            None,
        )?,
        SampleFormat::U16 => device.build_output_stream(
            &config,
            move |data: &mut [u16], _| {
                fill(data, channels, &buffer, |sample| ((sample.clamp(-1.0, 1.0) + 1.0) * 32767.0) as u16)
            },
            |err| eprintln!("speaker error: {err}"),
            None,
        )?,
        other => anyhow::bail!("unsupported speaker format {other:?}"),
    };
    stream.play()?;
    Ok(stream)
}

fn fill<T: Copy>(
    data: &mut [T],
    channels: usize,
    buffer: &Mutex<VecDeque<f32>>,
    map: impl Fn(f32) -> T,
) {
    let channels = channels.max(1);
    let mut guard = buffer.lock().ok();
    for frame in data.chunks_mut(channels) {
        let sample = guard.as_mut().and_then(|queue| queue.pop_front()).unwrap_or(0.0);
        let mapped = map(sample);
        for slot in frame {
            *slot = mapped;
        }
    }
}
