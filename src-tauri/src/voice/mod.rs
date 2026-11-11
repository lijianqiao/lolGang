//! 语音（3.0，按住说话；计划 3.4）：按住说话键录音、编码、发给队友；队友的话收下来、排好、解码、混音、播放。
//!
//! 实现流程：
//! 1. 一个专门的线程拥有全部状态，别处只往它的通道里发消息（`VoiceMsg`）：手势交来的说话键、
//!    队友的语音帧、设置、试音。闲着时只等消息，有事时每 10 毫秒走一步；
//!    麦克风录到的声音由声卡线程追加到共享的缓冲区，每一步取走（`audio::MicBuffer`）。
//! 2. 说话：按下就开麦克风（`audio`），录到的声音是 16 kHz 单声道，每 20 毫秒编一个 Opus 小帧（16 kbps 恒定码率），
//!    攒够 400 毫秒打成一个语音帧（`packet`）交给节点发出去（局域网和中继都走）。
//!    点了一下或在打字（手势说「丢掉」）就关麦克风、什么都不发；400 毫秒内松开的第一帧还没发，也就不会有半个字。
//!    松开时把剩下的发出去并标上「结束」；最长 30 秒，到时自动结束。
//! 3. 播放：每个人的每段话一个抖动缓冲（`jitter`）和一个 Opus 解码器。扬声器的队列里保持约 60 毫秒的声音：
//!    不够时从每段话各取 20 毫秒、解码（丢了的让解码器补）、乘上语音音量、加起来，转成扬声器的采样率放进去。
//!    所以放的快慢跟着声卡的时钟走。没人说话 3 秒后关掉扬声器。
//!    混音、解码、重采样用的缓冲区都留着复用，每 20 毫秒不再新分配（3.1.1）。
//! 4. 显示：自己在录时透明窗口右下角显示「正在录音」和剩余秒数；队友在说时显示他的名字（队伍面板里他那一行也亮起），
//!    变了才通知。
//! 5. 试音：录 3 秒，在本机编码再解码放给自己听（不发给任何人），走的是和队友语音一样的路。
//! 6. 关了语音：说话键本来就不拦（手势配置），收到的语音也不放。
//! 7. 历史记录（3.0-M4）：自己说完的一段、队友放完的一段，整段 Opus 小帧交给 `history` 存下；
//!    聊天框点了播放，就把那一段当成一段本机的话放出来（和试音一样不发给任何人），放完或点了停止告诉聊天框。

pub mod audio;
pub mod jitter;
pub mod packet;
pub mod resample;

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use opus_rs::{Application, OpusDecoder, OpusEncoder};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::api::AppState;
use crate::crypto::MemberId;
use crate::input::TalkPhase;
use crate::input::gesture::TALK_MIN_MS;
use crate::node::now_ms;
use crate::overlay::Msg;
use crate::panel::PanelMsg;
use jitter::{Jitter, Pull};
use packet::Packer;
use resample::Resampler;

/// 语音编码的采样率（单声道）。
pub const RATE: u32 = 16_000;
/// 一个小帧 20 毫秒。
const FRAME: usize = 320;
const BITRATE: i32 = 16_000;
/// 一次最长说多久。
pub const MAX_TALK_MS: u64 = 30_000;
/// 试音录多久。
const TEST_MS: u64 = 3_000;
/// 扬声器队列里保持多少声音：够盖住线程调度的抖动，又不加太多延迟。
const AHEAD_MS: usize = 60;
/// 没人说话多久后关掉扬声器。
const SPEAKER_IDLE: Duration = Duration::from_secs(3);
/// 同时最多放这么多段话（队伍最多 16 人，留余量）。
const MAX_STREAMS: usize = 32;
const TICK: Duration = Duration::from_millis(10);
/// 本机放的声音（试音、回放历史记录）的「说话人」：不会和真正的公钥撞。
const LOCAL: MemberId = [0; 32];
const TEST_KEY: (MemberId, u32) = (LOCAL, 0);
const PLAYBACK_KEY: (MemberId, u32) = (LOCAL, 1);
/// 队友一段话最多存这么多个小帧（30 秒是 1500 个，留余量）。
const MAX_RECORDED: usize = 2000;
/// 聊天框的窗口名。
const CHAT_WINDOW: &str = "text";

pub enum VoiceMsg {
    Talk(TalkPhase),
    /// 队友的一个语音帧（已验签、是队员）。
    Remote {
        from: MemberId,
        stream: u32,
        first: u32,
        frames: Vec<Vec<u8>>,
        end: bool,
    },
    /// 队员退出程序或离开队伍。
    Gone(MemberId),
    Config(VoiceConfig),
    /// 试音：录 3 秒放给自己听。
    Test,
    /// 回放历史记录里的一段语音（只在本机放）。
    Play {
        id: i64,
        frames: Vec<Vec<u8>>,
    },
    /// 停止回放。
    StopPlay,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VoiceConfig {
    pub enabled: bool,
    /// 0 到 1。
    pub volume: f32,
    /// 麦克风的设备 id；空表示系统默认。
    pub mic: String,
}

/// 语音页要显示的状态。
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceStatus {
    /// 麦克风或扬声器打不开的原因；好了就是 null。
    pub error: Option<String>,
    /// 试音：recording、playing；没在试是 null。
    pub test: Option<&'static str>,
}

#[derive(Clone)]
pub struct Voice {
    tx: Sender<VoiceMsg>,
    status: Arc<Mutex<VoiceStatus>>,
}

impl Voice {
    pub fn start(app: &AppHandle, config: VoiceConfig) -> Self {
        let (tx, rx) = mpsc::channel();
        let status = Arc::new(Mutex::new(VoiceStatus::default()));
        let runner = Runner {
            app: app.clone(),
            status: status.clone(),
            config,
            rec: None,
            take: 0,
            streams: HashMap::new(),
            speaker: None,
            idle_since: None,
            shown: (None, Vec::new()),
            playing: None,
            mixed: vec![0.0; FRAME],
            decoded: vec![0.0; FRAME],
            resampled: Vec::new(),
        };
        std::thread::Builder::new()
            .name("voice".into())
            .spawn(move || runner.run(rx))
            .expect("能开线程");
        Self { tx, status }
    }

    pub fn send(&self, m: VoiceMsg) {
        let _ = self.tx.send(m);
    }

    pub fn status(&self) -> VoiceStatus {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Talk,
    Test,
}

/// 正在录的一段：说话或试音。
struct Recording {
    purpose: Purpose,
    _mic: cpal::Stream,
    /// 声卡线程往里加录到的声音，每一步取到 `pcm` 里。
    captured: audio::MicBuffer,
    since: Instant,
    /// 开始录的时间（Unix 毫秒），记进历史记录。
    started_ms: u64,
    /// 取来还没编码的声音（不满 20 毫秒的留到下次）。
    pcm: Vec<f32>,
    encoder: OpusEncoder,
    packer: Packer,
    /// 录下的小帧：试音的放给自己听，说话的放完存进历史记录。
    kept: Vec<Vec<u8>>,
}

struct Incoming {
    jitter: Jitter,
    decoder: OpusDecoder,
    /// 收到的小帧（按序号），这段话放完后存进历史记录。
    recorded: std::collections::BTreeMap<u32, Vec<u8>>,
    started_ms: u64,
}

struct Runner {
    app: AppHandle,
    status: Arc<Mutex<VoiceStatus>>,
    config: VoiceConfig,
    rec: Option<Recording>,
    take: u32,
    streams: HashMap<(MemberId, u32), Incoming>,
    speaker: Option<(audio::Speaker, Resampler)>,
    idle_since: Option<Instant>,
    /// 上一次交给透明窗口的：自己还剩几秒、谁在说。
    shown: (Option<u64>, Vec<MemberId>),
    /// 正在回放的历史记录。
    playing: Option<i64>,
    /// 混好的 20 毫秒、一个解码器解出来的 20 毫秒、转成扬声器采样率的：每次复用。
    mixed: Vec<f32>,
    decoded: Vec<f32>,
    resampled: Vec<f32>,
}

impl Runner {
    fn run(mut self, rx: Receiver<VoiceMsg>) {
        loop {
            let busy = self.rec.is_some() || !self.streams.is_empty() || self.speaker.is_some();
            let msg = if busy {
                match rx.recv_timeout(TICK) {
                    Ok(m) => Some(m),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            } else {
                match rx.recv() {
                    Ok(m) => Some(m),
                    Err(_) => return,
                }
            };
            if let Some(m) = msg {
                self.handle(m);
            }
            self.step();
        }
    }

    fn state(&self) -> Option<tauri::State<'_, AppState>> {
        self.app.try_state::<AppState>()
    }

    fn handle(&mut self, m: VoiceMsg) {
        match m {
            VoiceMsg::Talk(TalkPhase::Start) => {
                if self.config.enabled && self.rec.is_none() {
                    self.start_recording(Purpose::Talk);
                }
            }
            VoiceMsg::Talk(TalkPhase::End) => {
                if self
                    .rec
                    .as_ref()
                    .is_some_and(|r| r.purpose == Purpose::Talk)
                {
                    self.finish_talk();
                }
            }
            VoiceMsg::Talk(TalkPhase::Cancel) => {
                if self
                    .rec
                    .as_ref()
                    .is_some_and(|r| r.purpose == Purpose::Talk)
                {
                    self.rec = None;
                }
            }
            VoiceMsg::Remote {
                from,
                stream,
                first,
                frames,
                end,
            } => {
                if !self.config.enabled || !frames_in_range(first, frames.len()) {
                    return;
                }
                let key = (from, stream);
                if !self.streams.contains_key(&key) && self.streams.len() >= MAX_STREAMS {
                    return;
                }
                let Some(s) = self.incoming(key) else { return };
                for (k, f) in frames.iter().enumerate() {
                    if s.recorded.len() < MAX_RECORDED {
                        s.recorded.insert(first + k as u32, f.clone());
                    }
                }
                s.jitter.push(first, frames, end, now_ms());
            }
            VoiceMsg::Gone(from) => {
                let gone: Vec<_> = self
                    .streams
                    .keys()
                    .filter(|(m, _)| *m == from)
                    .copied()
                    .collect();
                for key in gone {
                    if let Some(s) = self.streams.remove(&key) {
                        self.archive(key, s);
                    }
                }
            }
            VoiceMsg::Config(c) => {
                if !c.enabled {
                    self.streams.clear();
                    self.set_playing(None);
                    if self
                        .rec
                        .as_ref()
                        .is_some_and(|r| r.purpose == Purpose::Talk)
                    {
                        self.rec = None;
                    }
                }
                self.config = c;
            }
            VoiceMsg::Test => {
                if self.rec.is_none() {
                    self.start_recording(Purpose::Test);
                }
            }
            VoiceMsg::Play { id, frames } => {
                self.streams.remove(&PLAYBACK_KEY);
                if let Some(s) = self.incoming(PLAYBACK_KEY) {
                    s.jitter.push(0, frames, true, now_ms());
                    self.set_playing(Some(id));
                }
            }
            VoiceMsg::StopPlay => {
                self.streams.remove(&PLAYBACK_KEY);
                self.set_playing(None);
            }
        }
    }

    /// 告诉聊天框正在回放哪一条（None：没在放）。
    fn set_playing(&mut self, id: Option<i64>) {
        if self.playing != id {
            self.playing = id;
            let _ = self.app.emit_to(CHAT_WINDOW, "voice:playing", id);
        }
    }

    /// 队友的一段话放完了（或者他走了）：收到的小帧按顺序存进历史记录。
    fn archive(&self, key: (MemberId, u32), s: Incoming) {
        if key.0 == LOCAL || s.recorded.is_empty() {
            return;
        }
        if let Some(state) = self.state() {
            state.history.add(crate::history::NewEntry {
                member: Some(key.0),
                ts_ms: s.started_ms,
                kind: crate::history::Kind::Voice,
                text: String::new(),
                voice: Some(s.recorded.into_values().collect()),
            });
        }
    }

    fn incoming(&mut self, key: (MemberId, u32)) -> Option<&mut Incoming> {
        match self.streams.entry(key) {
            Entry::Occupied(e) => Some(e.into_mut()),
            Entry::Vacant(v) => {
                let decoder = OpusDecoder::new(RATE as i32, 1).ok()?;
                Some(v.insert(Incoming {
                    jitter: Jitter::default(),
                    decoder,
                    recorded: std::collections::BTreeMap::new(),
                    started_ms: now_ms(),
                }))
            }
        }
    }

    fn set_status(&self, f: impl FnOnce(&mut VoiceStatus)) {
        let snapshot = {
            let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut s);
            s.clone()
        };
        let _ = self.app.emit_to("main", "voice://changed", snapshot);
    }

    fn start_recording(&mut self, purpose: Purpose) {
        self.take = self.take.wrapping_add(1);
        let take = self.take;
        // 预留 1 秒：语音线程每 10 毫秒取一次，正常不会变大
        let captured: audio::MicBuffer = Arc::new(Mutex::new(Vec::with_capacity(RATE as usize)));
        let mic = audio::open_mic(&self.config.mic, captured.clone());
        let mic = match mic {
            Ok(m) => m,
            Err(e) => {
                let error = crate::tr!(format!("麦克风打不开：{e}"), format!("麦克风打不开：{e}"));
                if let Some(s) = self.state() {
                    s.overlays.send(Msg::Toast {
                        title: crate::tr!("麦克风打不开", "麦克风打不开").into(),
                        body: e.clone(),
                    });
                }
                self.set_status(|s| s.error = Some(error));
                return;
            }
        };
        let Ok(mut encoder) = OpusEncoder::new(RATE as i32, 1, Application::Voip) else {
            return;
        };
        encoder.bitrate_bps = BITRATE;
        encoder.use_cbr = true;
        self.rec = Some(Recording {
            purpose,
            _mic: mic,
            captured,
            since: Instant::now(),
            started_ms: now_ms(),
            pcm: Vec::with_capacity(RATE as usize),
            encoder,
            packer: Packer::new(getrandom::u32().unwrap_or(take)),
            kept: Vec::new(),
        });
        self.set_status(|s| {
            s.error = None;
            if purpose == Purpose::Test {
                s.test = Some("recording");
            }
        });
    }

    /// 取走录到的声音：凑够 20 毫秒就编一个小帧；说话的攒够 400 毫秒就发出去。
    fn drain_mic(&mut self) {
        let Some(rec) = self.rec.as_mut() else {
            return;
        };
        {
            let mut captured = rec.captured.lock().unwrap_or_else(|e| e.into_inner());
            rec.pcm.extend_from_slice(&captured);
            captured.clear();
        }
        let mut out = Vec::new();
        let mut used = 0;
        while rec.pcm.len() - used >= FRAME {
            let frame = &rec.pcm[used..used + FRAME];
            used += FRAME;
            let Some(packet) = encode(&mut rec.encoder, frame) else {
                continue;
            };
            match rec.purpose {
                Purpose::Talk => {
                    rec.kept.push(packet.clone());
                    out.extend(rec.packer.push(packet));
                }
                Purpose::Test => rec.kept.push(packet),
            }
        }
        rec.pcm.drain(..used);
        for body in out {
            self.send_live(body);
        }
    }

    fn send_live(&self, body: crate::protocol::live::LiveBody) {
        if let Some(s) = self.state() {
            s.node.send_live(body);
        }
    }

    /// 说完了：剩下的（不满 20 毫秒的补静音）编完，和「结束」一起发出去。还没发过东西、说得又太短就算了。
    fn finish_talk(&mut self) {
        self.drain_mic();
        let Some(mut rec) = self.rec.take() else {
            return;
        };
        if rec.packer.sent() == 0 && rec.since.elapsed() < Duration::from_millis(TALK_MIN_MS) {
            return;
        }
        let mut out = Vec::new();
        if !rec.pcm.is_empty() {
            rec.pcm.resize(FRAME, 0.0);
            if let Some(p) = encode(&mut rec.encoder, &rec.pcm) {
                rec.kept.push(p.clone());
                out.extend(rec.packer.push(p));
            }
        }
        out.push(rec.packer.finish());
        for body in out {
            self.send_live(body);
        }
        if let Some(state) = self.state()
            && !rec.kept.is_empty()
        {
            state.history.add(crate::history::NewEntry {
                member: None,
                ts_ms: rec.started_ms,
                kind: crate::history::Kind::Voice,
                text: String::new(),
                voice: Some(rec.kept),
            });
        }
    }

    /// 试音录完：把录下的小帧当成一段本机的话放出来。
    fn finish_test(&mut self) {
        let Some(rec) = self.rec.take() else { return };
        let count = rec.kept.len();
        if let Some(s) = self.incoming(TEST_KEY) {
            s.jitter = Jitter::default();
            s.jitter.push(0, rec.kept, true, now_ms());
        }
        self.set_status(|s| s.test = (count > 0).then_some("playing"));
    }

    fn step(&mut self) {
        self.drain_mic();
        if let Some(rec) = &self.rec {
            let elapsed = rec.since.elapsed();
            match rec.purpose {
                Purpose::Talk if elapsed >= Duration::from_millis(MAX_TALK_MS) => {
                    self.finish_talk()
                }
                Purpose::Test if elapsed >= Duration::from_millis(TEST_MS) => self.finish_test(),
                _ => {}
            }
        }
        self.play();
        self.show();
    }

    /// 让扬声器的队列里保持约 60 毫秒的声音。
    fn play(&mut self) {
        if self.streams.is_empty() {
            let drained = self
                .speaker
                .as_ref()
                .is_none_or(|(s, _)| s.queue.lock().map(|q| q.is_empty()).unwrap_or(true));
            if drained && self.speaker.is_some() {
                let since = *self.idle_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= SPEAKER_IDLE {
                    self.speaker = None;
                    self.idle_since = None;
                }
            }
            return;
        }
        self.idle_since = None;
        if self.speaker.is_none() {
            match audio::open_speaker() {
                Ok(s) => {
                    let r = Resampler::new(RATE, s.rate);
                    self.speaker = Some((s, r));
                }
                Err(e) => {
                    self.streams.clear();
                    let error =
                        crate::tr!(format!("扬声器打不开：{e}"), format!("扬声器打不开：{e}"));
                    self.set_status(|s| {
                        s.error = Some(error);
                        s.test = None;
                    });
                    return;
                }
            }
        }
        let now = now_ms();
        let Some((speaker, _)) = &self.speaker else {
            return;
        };
        let target = speaker.rate as usize * AHEAD_MS / 1000;
        let queue = speaker.queue.clone();
        while !self.streams.is_empty() && queue.lock().map(|q| q.len()).unwrap_or(target) < target {
            self.mix(now);
            let Some((_, resampler)) = self.speaker.as_mut() else {
                return;
            };
            self.resampled.clear();
            resampler.process(&self.mixed, &mut self.resampled);
            queue
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend(&self.resampled);
        }
    }

    /// 每段话各取 20 毫秒，解码、加起来放进 `mixed`；放完的去掉。
    fn mix(&mut self, now: u64) {
        self.mixed.fill(0.0);
        let (sum, decoded) = (&mut self.mixed, &mut self.decoded);
        let volume = self.config.volume;
        let mut done = Vec::new();
        for (key, s) in &mut self.streams {
            let input: &[u8] = match s.jitter.pull(now) {
                Pull::Frame(f) => {
                    let n = s.decoder.decode(&f, FRAME, decoded).unwrap_or(0);
                    add(sum, &decoded[..n.min(FRAME)], volume);
                    continue;
                }
                Pull::Lost => &[],
                Pull::Wait => continue,
                Pull::Done => {
                    done.push(*key);
                    continue;
                }
            };
            let n = s.decoder.decode(input, FRAME, decoded).unwrap_or(0);
            add(sum, &decoded[..n.min(FRAME)], volume);
        }
        for v in sum.iter_mut() {
            *v = v.clamp(-1.0, 1.0);
        }
        for key in done {
            let Some(s) = self.streams.remove(&key) else {
                continue;
            };
            match key {
                TEST_KEY => self.set_status(|st| st.test = None),
                PLAYBACK_KEY => self.set_playing(None),
                _ => self.archive(key, s),
            }
        }
    }

    /// 透明窗口上的「正在录音」「谁在说」，队伍面板里亮起的人；变了才通知。
    fn show(&mut self) {
        let me = self
            .rec
            .as_ref()
            .filter(|r| r.purpose == Purpose::Talk)
            .map(|r| (MAX_TALK_MS / 1000).saturating_sub(r.since.elapsed().as_secs()));
        let mut others: Vec<MemberId> = self
            .streams
            .iter()
            .filter(|((m, _), s)| *m != LOCAL && s.jitter.speaking())
            .map(|((m, _), _)| *m)
            .collect();
        others.sort();
        others.dedup();
        if (me, &others) == (self.shown.0, &self.shown.1) {
            return;
        }
        if let Some(s) = self.state() {
            s.overlays.send(Msg::Speaking {
                me,
                others: others.clone(),
            });
            if others != self.shown.1 {
                s.panel.send(PanelMsg::Speaking(others.clone()));
            }
        }
        self.shown = (me, others);
    }
}

fn encode(encoder: &mut OpusEncoder, pcm: &[f32]) -> Option<Vec<u8>> {
    let mut buf = [0u8; 400];
    let n = encoder.encode(pcm, FRAME, &mut buf).ok()?;
    Some(buf[..n].to_vec())
}

/// 队友的一段话最多 30 秒：小帧序号超出 `MAX_RECORDED` 的是坏包（3.2.0）。不拦的话序号相加会溢出，
/// 两个序号隔得很远的包还会让抖动缓冲把中间的空档一个个当「丢了」补下去，这段话就一直放不完。
fn frames_in_range(first: u32, count: usize) -> bool {
    first as usize + count <= MAX_RECORDED
}

fn add(sum: &mut [f32], pcm: &[f32], volume: f32) {
    for (s, p) in sum.iter_mut().zip(pcm) {
        *s += p * volume;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 编码、打包、按到达顺序进抖动缓冲、解码：一段话从头到尾走一遍，声音还在。
    #[test]
    fn a_phrase_survives_encode_pack_jitter_decode() {
        let mut enc = OpusEncoder::new(RATE as i32, 1, Application::Voip).unwrap();
        enc.bitrate_bps = BITRATE;
        enc.use_cbr = true;
        let mut packer = Packer::new(3);
        let mut bodies = Vec::new();
        let tone: Vec<f32> = (0..RATE as usize)
            .map(|i| (i as f32 * 300.0 * 2.0 * std::f32::consts::PI / RATE as f32).sin() * 0.4)
            .collect();
        for chunk in tone.chunks(FRAME) {
            let p = encode(&mut enc, chunk).unwrap();
            assert_eq!(p.len(), 40, "16 kbps 恒定码率：每 20 毫秒 40 字节");
            bodies.extend(packer.push(p));
        }
        bodies.push(packer.finish());
        assert_eq!(
            bodies.len(),
            3,
            "1 秒 = 两个 400 毫秒的帧，剩下的 200 毫秒和「结束」一起发"
        );

        let mut j = Jitter::default();
        for b in bodies {
            if let crate::protocol::live::LiveBody::Voice {
                first, frames, end, ..
            } = b
            {
                j.push(
                    first,
                    frames.into_iter().map(|f| f.into_vec()).collect(),
                    end,
                    0,
                );
            }
        }
        let mut dec = OpusDecoder::new(RATE as i32, 1).unwrap();
        let mut out = vec![0f32; FRAME];
        let (mut energy, mut frames) = (0f64, 0);
        loop {
            match j.pull(0) {
                Pull::Frame(f) => {
                    let n = dec.decode(&f, FRAME, &mut out).unwrap();
                    energy += out[..n].iter().map(|s| f64::from(*s).powi(2)).sum::<f64>();
                    frames += 1;
                }
                Pull::Done => break,
                p => panic!("不该是 {p:?}"),
            }
        }
        assert_eq!(frames, 50);
        let input: f64 = tone.iter().map(|s| f64::from(*s).powi(2)).sum();
        assert!(energy / input > 0.5, "解码后的能量 {:.2}", energy / input);
    }

    #[test]
    fn frame_numbers_past_a_talk_are_refused() {
        assert!(frames_in_range(0, 20));
        assert!(frames_in_range((MAX_RECORDED - 20) as u32, 20));
        assert!(!frames_in_range((MAX_RECORDED - 19) as u32, 20));
        assert!(!frames_in_range(u32::MAX, 20));
    }

    /// 队友发来的语音帧只验了签名，内容可能是坏的：每种帧头、随机字节、改坏的真帧，解码都不能 panic
    /// （正式版 panic 直接退出）。
    #[test]
    fn malformed_voice_frames_do_not_panic_the_decoder() {
        let mut enc = OpusEncoder::new(RATE as i32, 1, Application::Voip).unwrap();
        enc.bitrate_bps = BITRATE;
        enc.use_cbr = true;
        let real: Vec<Vec<u8>> = (0..10)
            .map(|k| {
                let pcm: Vec<f32> = (0..FRAME)
                    .map(|i| ((i + k * FRAME) as f32 * 0.12).sin() * 0.5)
                    .collect();
                encode(&mut enc, &pcm).unwrap()
            })
            .collect();
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut inputs: Vec<Vec<u8>> = Vec::new();
        // 帧头（TOC）的 32 种配置 × 单双声道 × 4 种帧数写法，后面跟随机内容，长短都有
        for toc in 0..=255u8 {
            for len in [1usize, 2, 3, 5, 40, 120, 400] {
                let mut p = vec![toc];
                p.extend((1..len).map(|_| rnd() as u8));
                inputs.push(p);
            }
        }
        for _ in 0..5000 {
            let len = (rnd() % 401) as usize;
            inputs.push((0..len).map(|_| rnd() as u8).collect());
        }
        for f in &real {
            for _ in 0..300 {
                let mut p = f.clone();
                let at = (rnd() as usize) % p.len();
                p[at] ^= 1 << (rnd() % 8);
                if rnd() % 3 == 0 {
                    p.truncate((rnd() as usize) % p.len());
                }
                inputs.push(p);
            }
        }
        // 同一个解码器一直用下去：坏帧留下的状态也不能让后面的帧出事
        let mut dec = OpusDecoder::new(RATE as i32, 1).unwrap();
        let mut out = vec![0f32; FRAME];
        for p in &inputs {
            let n = dec.decode(p, FRAME, &mut out).unwrap_or(0);
            assert!(n <= FRAME, "{p:02x?} 解出 {n} 个样本，超过缓冲");
        }
    }
}
