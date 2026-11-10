//! 历史记录的运行时（3.0，计划 3.6）：信号、表情、文字、语音发生时记下来，聊天框打开时读出来。
//!
//! 实现流程：
//! 1. 一个专门的线程拥有自己的数据库连接（同一个 `gang2.db`，WAL 模式下和别的连接互不阻塞），
//!    别处只往通道里发消息，所以节点任务、透明窗口线程、语音线程记一条都不用等数据库。
//! 2. 记在「现在所在的队伍」下面：队伍有变化时（`team::refresh_names`）告诉这里现在是哪个队伍，
//!    别的队伍的记录当场删掉（`storage::history::keep_only`）——退出、被踢、解散（包括不在线时被解散、上线后才同步到）都是这一条。
//! 3. 加一条：存库、该修剪时修剪（每个队伍 2000 条、语音总共 100 MB），再推给聊天框（"history:added"），开着的聊天框直接加上这一条。
//!    条数和语音总字节数记在内存里（`Counts`，换队伍时从库里数一次），不用每加一条都扫全表（3.1.1）：
//!    条数超出上限 `TRIM_MARGIN` 条才一次删回上限；语音一超就从最旧的删。删过就从库里重新数。
//! 4. 语音存成一串 Opus 小帧：每个前面两字节长度（小端），回放时拆开交给语音线程。

use std::sync::mpsc::{self, Receiver, Sender};

use rusqlite::Connection;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::oneshot;

use crate::crypto::{MemberId, RoomId};
use crate::error::AppResult;
use crate::protocol::live::LiveBody;
pub use crate::storage::history::Kind;
use crate::storage::history::{self as store, Row};

/// 每个队伍最多留多少条。
pub const MAX_ROWS: usize = 2000;
/// 条数超出上限这么多才修剪一次。
const TRIM_MARGIN: usize = 50;
/// 所有队伍的语音加起来最多这么多字节。
pub const MAX_VOICE_BYTES: u64 = 100 * 1024 * 1024;
/// 一个 Opus 小帧多长（毫秒）。
const FRAME_MS: u32 = 20;
/// 聊天框的窗口名（`panel.rs` 建的输入框窗口）。
const CHAT_WINDOW: &str = "text";

/// 要记的一条。`member` 为 None 表示是我自己。
pub struct NewEntry {
    pub member: Option<MemberId>,
    pub ts_ms: u64,
    pub kind: Kind,
    /// 文字的内容、信号 id、表情引用。
    pub text: String,
    /// 语音的 Opus 小帧（每个 20 毫秒）。
    pub voice: Option<Vec<Vec<u8>>>,
}

/// 聊天框里的一条。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryDto {
    pub id: i64,
    /// 公钥的十六进制。
    pub member_id: String,
    pub me: bool,
    /// 名字的颜色，和信号标签一样。
    pub color: &'static str,
    pub ts: u64,
    /// text、voice、ping、emote。
    pub kind: &'static str,
    pub text: String,
    pub voice_ms: u32,
}

enum Msg {
    Room(Option<RoomId>),
    Add(NewEntry),
    List(oneshot::Sender<Vec<EntryDto>>),
    Voice(i64, oneshot::Sender<Option<Vec<Vec<u8>>>>),
}

#[derive(Clone)]
pub struct History {
    tx: Sender<Msg>,
}

impl History {
    pub fn start(app: &AppHandle, conn: Connection, me: MemberId) -> Self {
        let (tx, rx) = mpsc::channel();
        let runner = Runner {
            app: app.clone(),
            conn,
            me,
            room: None,
            counts: Counts::default(),
        };
        std::thread::Builder::new()
            .name("history".into())
            .spawn(move || runner.run(rx))
            .expect("能开线程");
        Self { tx }
    }

    /// 现在所在的队伍（加入中或已加入）；None 表示不在队伍里。别的队伍的记录会被删掉。
    pub fn set_room(&self, room: Option<RoomId>) {
        let _ = self.tx.send(Msg::Room(room));
    }

    pub fn add(&self, e: NewEntry) {
        let _ = self.tx.send(Msg::Add(e));
    }

    /// 自己放的信号或表情（别的实时帧不记）。
    pub fn own_live(&self, body: &LiveBody) {
        if let Some((kind, text)) = live_entry(body) {
            self.add(NewEntry {
                member: None,
                ts_ms: crate::node::now_ms(),
                kind,
                text,
                voice: None,
            });
        }
    }

    /// 队友放的信号或表情。
    pub fn remote_live(&self, from: MemberId, body: &LiveBody) {
        if let Some((kind, text)) = live_entry(body) {
            self.add(NewEntry {
                member: Some(from),
                ts_ms: crate::node::now_ms(),
                kind,
                text,
                voice: None,
            });
        }
    }

    /// 现在所在队伍的全部记录，从旧到新。异步等回话（3.1.1：聊天框的命令不再在主线程上等）。
    pub async fn list(&self) -> Vec<EntryDto> {
        let (tx, rx) = oneshot::channel();
        let _ = self.tx.send(Msg::List(tx));
        rx.await.unwrap_or_default()
    }

    /// 一条语音的 Opus 小帧。
    pub async fn voice(&self, id: i64) -> Option<Vec<Vec<u8>>> {
        let (tx, rx) = oneshot::channel();
        let _ = self.tx.send(Msg::Voice(id, tx));
        rx.await.ok().flatten()
    }
}

fn live_entry(body: &LiveBody) -> Option<(Kind, String)> {
    match body {
        LiveBody::Ping { id, .. } => Some((Kind::Ping, id.clone())),
        LiveBody::Emote { emote, .. } => Some((Kind::Emote, emote.clone())),
        _ => None,
    }
}

struct Runner {
    app: AppHandle,
    conn: Connection,
    me: MemberId,
    /// 现在所在的队伍；还没听说过时是 None（这时不记）。
    room: Option<RoomId>,
    counts: Counts,
}

/// 内存里记着的条数和语音总字节数，加一条时不用扫全表（3.1.1）。
#[derive(Debug, Default, PartialEq, Eq)]
struct Counts {
    /// 现在所在队伍有几条（库里也只有这个队伍的记录）。
    rows: usize,
    /// 库里的语音一共多少字节。
    voice_bytes: u64,
}

impl Counts {
    /// 从库里数（换队伍时，或者修剪出错时）。
    fn load(conn: &Connection, room: Option<&RoomId>) -> Self {
        Self {
            rows: room.and_then(|r| store::count(conn, r).ok()).unwrap_or(0),
            voice_bytes: store::voice_bytes(conn).unwrap_or(0),
        }
    }

    /// 加了一条（语音 `voice_len` 字节）之后：条数超出上限 `TRIM_MARGIN` 条就删回 `max_rows`，
    /// 语音超了从最旧的删；删过就从库里重新数（删的条里可能有语音，删的语音也算条数）。
    fn added(
        &mut self,
        conn: &Connection,
        room: &RoomId,
        voice_len: u64,
        max_rows: usize,
        max_voice_bytes: u64,
    ) -> AppResult<()> {
        self.rows += 1;
        self.voice_bytes += voice_len;
        let mut trimmed = false;
        if self.rows > max_rows + TRIM_MARGIN {
            store::trim_rows(conn, room, max_rows)?;
            self.voice_bytes = store::voice_bytes(conn)?;
            trimmed = true;
        }
        if self.voice_bytes > max_voice_bytes {
            self.voice_bytes = store::trim_voice(conn, self.voice_bytes, max_voice_bytes)?;
            trimmed = true;
        }
        if trimmed {
            self.rows = store::count(conn, room)?;
        }
        Ok(())
    }
}

impl Runner {
    fn run(mut self, rx: Receiver<Msg>) {
        while let Ok(m) = rx.recv() {
            match m {
                Msg::Room(room) => {
                    self.room = room;
                    if let Err(e) = store::keep_only(&self.conn, room.as_ref()) {
                        eprintln!("历史记录没删掉：{e}");
                    }
                    self.counts = Counts::load(&self.conn, room.as_ref());
                }
                Msg::Add(e) => self.add(e),
                Msg::List(reply) => {
                    let rows = self
                        .room
                        .and_then(|r| store::list(&self.conn, &r).ok())
                        .unwrap_or_default();
                    let _ = reply.send(rows.iter().map(|r| self.dto(r)).collect());
                }
                Msg::Voice(id, reply) => {
                    let frames = store::voice(&self.conn, id)
                        .ok()
                        .flatten()
                        .map(|b| unpack(&b));
                    let _ = reply.send(frames);
                }
            }
        }
    }

    fn add(&mut self, e: NewEntry) {
        let Some(room) = self.room else { return };
        let member = e.member.unwrap_or(self.me);
        let voice = e
            .voice
            .map(|frames| (pack(&frames), frames.len() as u32 * FRAME_MS));
        let added = store::add(
            &self.conn,
            &room,
            &member,
            e.ts_ms,
            e.kind,
            &e.text,
            voice.as_ref().map(|(b, ms)| (b.as_slice(), *ms)),
        );
        if added.is_ok() {
            let voice_len = voice.as_ref().map_or(0, |(b, _)| b.len() as u64);
            if let Err(err) =
                self.counts
                    .added(&self.conn, &room, voice_len, MAX_ROWS, MAX_VOICE_BYTES)
            {
                eprintln!("历史记录没修剪：{err}");
                self.counts = Counts::load(&self.conn, Some(&room));
            }
        }
        match added {
            Ok(id) => {
                let dto = self.dto(&Row {
                    id,
                    member_id: member,
                    ts_ms: e.ts_ms,
                    kind: e.kind,
                    text: e.text,
                    voice_ms: voice.map_or(0, |(_, ms)| ms),
                });
                let _ = self.app.emit_to(CHAT_WINDOW, "history:added", dto);
            }
            Err(err) => eprintln!("历史记录没存上：{err}"),
        }
    }

    fn dto(&self, r: &Row) -> EntryDto {
        EntryDto {
            id: r.id,
            member_id: crate::crypto::hex(&r.member_id),
            me: r.member_id == self.me,
            color: crate::overlay::tag_color(&r.member_id),
            ts: r.ts_ms,
            kind: match r.kind {
                Kind::Text => "text",
                Kind::Voice => "voice",
                Kind::Ping => "ping",
                Kind::Emote => "emote",
            },
            text: r.text.clone(),
            voice_ms: r.voice_ms,
        }
    }
}

/// 一串小帧 → 每个前面两字节长度（小端）。
fn pack(frames: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(frames.iter().map(|f| f.len() + 2).sum());
    for f in frames {
        let len = u16::try_from(f.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&f[..usize::from(len)]);
    }
    out
}

fn unpack(mut b: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while b.len() >= 2 {
        let len = usize::from(u16::from_le_bytes([b[0], b[1]]));
        let Some(frame) = b.get(2..2 + len) else {
            break;
        };
        out.push(frame.to_vec());
        b = &b[2 + len..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 随机输入（见 `fuzz.rs`）：库里存的语音弄坏了（长度写大了、截断了），拆包只是少拆出几帧，不越界。
    #[test]
    fn broken_voice_blobs_never_panic() {
        let samples = vec![
            pack(&[vec![1, 2, 3], vec![], vec![9; 300]]),
            pack(&vec![vec![7u8; 40]; 20]),
        ];
        crate::fuzz::run("历史记录里的语音", &samples, |b| {
            let frames = unpack(b);
            let total: usize = frames.iter().map(|f| f.len() + 2).sum();
            assert!(total <= b.len(), "拆出来的不会比存的多");
        });
    }

    #[test]
    fn voice_frames_pack_and_unpack() {
        let frames = vec![vec![1, 2, 3], vec![], vec![9; 300]];
        assert_eq!(unpack(&pack(&frames)), frames);
        // 截断的数据：读到哪算哪，不出错
        let mut b = pack(&frames);
        b.truncate(b.len() - 10);
        assert_eq!(unpack(&b), frames[..2].to_vec());
    }

    #[test]
    fn trims_only_past_the_margin_and_keeps_counts_right() {
        let c = crate::storage::open_in_memory().unwrap();
        let room: RoomId = [1; 16];
        let me: MemberId = [7; 32];
        let add = |counts: &mut Counts, voice: Option<&[u8]>| {
            store::add(&c, &room, &me, 0, Kind::Text, "", voice.map(|v| (v, 20))).unwrap();
            let len = voice.map_or(0, |v| v.len() as u64);
            counts.added(&c, &room, len, 2, 100).unwrap();
        };
        let mut counts = Counts::load(&c, Some(&room));
        assert_eq!(counts, Counts::default());
        for _ in 0..2 + TRIM_MARGIN {
            add(&mut counts, None);
        }
        assert_eq!(
            store::count(&c, &room).unwrap(),
            2 + TRIM_MARGIN,
            "没超出余量不删"
        );
        add(&mut counts, None);
        assert_eq!(counts.rows, 2, "超出余量一次删回上限");
        assert_eq!(store::count(&c, &room).unwrap(), 2);

        add(&mut counts, Some(&[0; 40]));
        add(&mut counts, Some(&[0; 40]));
        assert_eq!((counts.rows, counts.voice_bytes), (4, 80));
        add(&mut counts, Some(&[0; 40]));
        assert_eq!(
            (counts.rows, counts.voice_bytes),
            (4, 80),
            "语音一超就删最旧的一条"
        );
        assert_eq!(
            counts,
            Counts::load(&c, Some(&room)),
            "内存里的数和库里一致"
        );
    }

    #[test]
    fn only_pings_and_emotes_come_from_live_frames() {
        let ping = LiveBody::Ping {
            display: 1,
            x: 0,
            y: 0,
            id: "danger".into(),
        };
        assert_eq!(live_entry(&ping), Some((Kind::Ping, "danger".into())));
        let emote = LiveBody::Emote {
            display: 1,
            x: 0,
            y: 0,
            emote: "facepalm".into(),
            fx: 0,
        };
        assert_eq!(live_entry(&emote), Some((Kind::Emote, "facepalm".into())));
        assert_eq!(live_entry(&LiveBody::Bye), None);
    }
}
