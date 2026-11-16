use crate::audio_hub::{AudioHandle, LocalCmd};
use crate::pcm::{self, SAMPLE_RATE};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};


pub fn start(handle: AudioHandle) {
    let (mic_tx, mut mic_rx) = tokio::sync::mpsc::channel::<(u32, Vec<f32>)>(16);
    let bridge = handle.clone();
    tokio::spawn(async move {
        while let Some((rate, samples)) = mic_rx.recv().await {
            let resampled = pcm::resample_linear(&samples, rate, SAMPLE_RATE);
            bridge.mic("local", pcm::f32_to_i16(&resampled));
        }
    });
    if std::thread::Builder::new()
        .name("local-audio".into())
        .spawn(move || {
            if let Err(err) = run(handle, mic_tx) {
                log::warn!("local microphone is unavailable: {err:#}");
            }
        })
        .is_err()
    {
        log::warn!("could not start the local audio thread");
    }
}

fn run(
    handle: AudioHandle,
    mic_tx: tokio::sync::mpsc::Sender<(u32, Vec<f32>)>,
) -> anyhow::Result<()> {
    let host = cpal::default_host();
    let input = match host.default_input_device() {
        Some(device) => Some(open_input(&device, mic_tx)?),
        None => {
            log::warn!("no default microphone");
            None
        }
    };
    let play_buffer: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
    let output = match host.default_output_device() {
        Some(device) => Some(open_output(&device, play_buffer.clone())?),
        None => {
            log::warn!("no default speaker");
            None
        }
    };
    let (cmd_tx, cmd_rx) = std::sync::mpsc::sync_channel::<LocalCmd>(32);
    handle.register_local(cmd_tx);
    let output_rate = output.as_ref().map(|opened| opened.rate).unwrap_or(SAMPLE_RATE);
    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            LocalCmd::Clear => {
                if let Ok(mut buffer) = play_buffer.lock() {
                    buffer.clear();
                }
            }
            LocalCmd::Play(pcm) => {
                let floats = pcm::i16_to_f32(&pcm);
                let resampled = pcm::resample_linear(&floats, SAMPLE_RATE, output_rate);
                if let Ok(mut buffer) = play_buffer.lock() {
                    buffer.extend(resampled);
                    let cap = output_rate as usize * 2;
                    while buffer.len() > cap {
                        buffer.pop_front();
                    }
                }
            }
        }
    }
    drop(input);
    drop(output);
    Ok(())
}

struct Opened {
    _stream: Stream,
    rate: u32,
}

fn open_input(
    device: &cpal::Device,
    tx: tokio::sync::mpsc::Sender<(u32, Vec<f32>)>,
) -> anyhow::Result<Opened> {
    let supported = device.default_input_config()?;
    let rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    log::info!(
        "microphone {} at {rate} Hz",
        device.name().unwrap_or_else(|_| "input".into())
    );
    let stream = match format {
        SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _| {
                let _ = tx.try_send((rate, pcm::downmix(data, channels)));
            },
            |err| log::warn!("microphone error: {err}"),
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _| {
                let floats: Vec<f32> = data.iter().map(|sample| *sample as f32 / 32768.0).collect();
                let _ = tx.try_send((rate, pcm::downmix(&floats, channels)));
            },
            |err| log::warn!("microphone error: {err}"),
            None,
        )?,
        SampleFormat::U16 => device.build_input_stream(
            &config,
            move |data: &[u16], _| {
                let floats: Vec<f32> = data
                    .iter()
                    .map(|sample| (*sample as f32 / 32768.0) - 1.0)
                    .collect();
                let _ = tx.try_send((rate, pcm::downmix(&floats, channels)));
            },
            |err| log::warn!("microphone error: {err}"),
            None,
        )?,
        other => anyhow::bail!("unsupported microphone format {other:?}"),
    };
    stream.play()?;
    Ok(Opened {
        _stream: stream,
        rate,
    })
}

fn open_output(device: &cpal::Device, buffer: Arc<Mutex<VecDeque<f32>>>) -> anyhow::Result<Opened> {
    let supported = device.default_output_config()?;
    let rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    log::info!(
        "speaker {} at {rate} Hz",
        device.name().unwrap_or_else(|_| "output".into())
    );
    let stream = match format {
        SampleFormat::F32 => {
            let buffer = buffer.clone();
            device.build_output_stream(
                &config,
                move |data: &mut [f32], _| fill_f32(data, channels, &buffer),
                |err| log::warn!("speaker error: {err}"),
                None,
            )?
        }
        SampleFormat::I16 => {
            let buffer = buffer.clone();
            device.build_output_stream(
                &config,
                move |data: &mut [i16], _| {
                    fill_mapped(data, channels, &buffer, |sample| (sample.clamp(-1.0, 1.0) * 32767.0) as i16)
                },
                |err| log::warn!("speaker error: {err}"),
                None,
            )?
        }
        SampleFormat::U16 => {
            let buffer = buffer.clone();
            device.build_output_stream(
                &config,
                move |data: &mut [u16], _| {
                    fill_mapped(data, channels, &buffer, |sample| {
                        ((sample.clamp(-1.0, 1.0) + 1.0) * 32767.0) as u16
                    })
                },
                |err| log::warn!("speaker error: {err}"),
                None,
            )?
        }
        other => anyhow::bail!("unsupported speaker format {other:?}"),
    };
    stream.play()?;
    Ok(Opened {
        _stream: stream,
        rate,
    })
}

fn fill_f32(data: &mut [f32], channels: usize, buffer: &Mutex<VecDeque<f32>>) {
    let channels = channels.max(1);
    let mut guard = buffer.lock().ok();
    for frame in data.chunks_mut(channels) {
        let sample = guard.as_mut().and_then(|queue| queue.pop_front()).unwrap_or(0.0);
        for slot in frame {
            *slot = sample;
        }
    }
}

fn fill_mapped<T: Copy>(
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


