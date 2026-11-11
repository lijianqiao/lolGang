//! 声卡（cpal：Windows WASAPI、macOS CoreAudio）：列出麦克风、开麦克风录音、开扬声器放音。
//! 只有这个文件碰 cpal。
//!
//! 实现流程：
//! 1. 麦克风按保存的设备 id 找，找不到（拔了、换了）就用系统默认的。用设备自己的默认格式开，
//!    每块录到的声音在声卡线程里混成单声道、转成 16 kHz，追加到和语音线程共享的缓冲区（语音线程每 10 毫秒取走）。
//!    声卡线程里不分配内存（3.1.1：以前每块新建一个 Vec 发进通道）：缓冲区都是复用的，第一块之后不再变大。
//! 2. 扬声器用系统默认的输出设备和格式。语音线程把要放的声音（扬声器的采样率、单声道）放进共享队列，
//!    声卡线程每次从队列里取，复制到每个声道；队列空了就是静音。
//! 3. 返回的 `cpal::Stream` 丢掉就停：麦克风只在按住说话（或试音）时开着，系统的「麦克风在用」提示不会一直亮。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, SampleFormat, SizedSample, Stream, StreamConfig};
use serde::Serialize;

use super::RATE;
use super::resample::Resampler;

#[derive(Clone, Debug, Serialize)]
pub struct MicInfo {
    pub id: String,
    pub name: String,
}

pub fn mics() -> Vec<MicInfo> {
    let Ok(devices) = cpal::default_host().input_devices() else {
        return Vec::new();
    };
    devices
        .filter_map(|d| {
            Some(MicInfo {
                id: d.id().ok()?.to_string(),
                name: d.description().ok()?.name().to_string(),
            })
        })
        .collect()
}

fn mic_device(id: &str) -> Option<Device> {
    let host = cpal::default_host();
    if !id.is_empty()
        && let Ok(did) = id.parse::<cpal::DeviceId>()
        && let Some(d) = host.device_by_id(&did)
    {
        return Some(d);
    }
    host.default_input_device()
}

/// 麦克风录到的声音（16 kHz 单声道）：声卡线程往后加，语音线程从前面取走。
pub type MicBuffer = Arc<Mutex<Vec<f32>>>;

/// 开麦克风：录到的声音（16 kHz 单声道）追加到 `buffer`。
pub fn open_mic(id: &str, buffer: MicBuffer) -> Result<Stream, String> {
    let dev = mic_device(id).ok_or(crate::tr!("找不到麦克风", "No microphone found"))?;
    let cfg = dev.default_input_config().map_err(|e| e.to_string())?;
    let (channels, rate) = (usize::from(cfg.channels()).max(1), cfg.sample_rate());
    let config = cfg.config();
    let stream = match cfg.sample_format() {
        SampleFormat::F32 => build_mic::<f32>(&dev, config, channels, rate, buffer),
        SampleFormat::I16 => build_mic::<i16>(&dev, config, channels, rate, buffer),
        SampleFormat::U16 => build_mic::<u16>(&dev, config, channels, rate, buffer),
        SampleFormat::I32 => build_mic::<i32>(&dev, config, channels, rate, buffer),
        f => {
            return Err(crate::tr!(
                format!("麦克风的采样格式 {f} 不支持"),
                format!("The microphone's sample format {f} isn't supported")
            ));
        }
    }?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}

fn build_mic<T>(
    dev: &Device,
    config: StreamConfig,
    channels: usize,
    rate: u32,
    buffer: MicBuffer,
) -> Result<Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let mut resampler = Resampler::new(rate, RATE);
    let mut mono = Vec::new();
    dev.build_input_stream::<T, _, _>(
        config,
        move |data: &[T], _| {
            mono.clear();
            mono.extend(
                data.chunks(channels)
                    .map(|c| c.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / c.len() as f32),
            );
            let mut out = buffer.lock().unwrap_or_else(|e| e.into_inner());
            resampler.process(&mono, &mut out);
        },
        |e| eprintln!("麦克风出错：{e}"),
        None,
    )
    .map_err(|e| e.to_string())
}

/// 开着的扬声器：往 `queue` 里放这个采样率的单声道声音。
pub struct Speaker {
    _stream: Stream,
    pub rate: u32,
    pub queue: Arc<Mutex<VecDeque<f32>>>,
}

pub fn open_speaker() -> Result<Speaker, String> {
    let dev = cpal::default_host()
        .default_output_device()
        .ok_or(crate::tr!("找不到扬声器", "No speaker found"))?;
    let cfg = dev.default_output_config().map_err(|e| e.to_string())?;
    let (channels, rate) = (usize::from(cfg.channels()).max(1), cfg.sample_rate());
    let config = cfg.config();
    let queue = Arc::new(Mutex::new(VecDeque::new()));
    let q = queue.clone();
    let stream = match cfg.sample_format() {
        SampleFormat::F32 => build_speaker::<f32>(&dev, config, channels, q),
        SampleFormat::I16 => build_speaker::<i16>(&dev, config, channels, q),
        SampleFormat::U16 => build_speaker::<u16>(&dev, config, channels, q),
        SampleFormat::I32 => build_speaker::<i32>(&dev, config, channels, q),
        f => {
            return Err(crate::tr!(
                format!("扬声器的采样格式 {f} 不支持"),
                format!("The speaker's sample format {f} isn't supported")
            ));
        }
    }?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(Speaker {
        _stream: stream,
        rate,
        queue,
    })
}

fn build_speaker<T>(
    dev: &Device,
    config: StreamConfig,
    channels: usize,
    queue: Arc<Mutex<VecDeque<f32>>>,
) -> Result<Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    dev.build_output_stream::<T, _, _>(
        config,
        move |data: &mut [T], _| {
            let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
            for frame in data.chunks_mut(channels) {
                let v = T::from_sample(q.pop_front().unwrap_or(0.0));
                frame.fill(v);
            }
        },
        |e| eprintln!("扬声器出错：{e}"),
        None,
    )
    .map_err(|e| e.to_string())
}
