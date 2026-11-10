//! 随机输入测试（3.3.0，只在测试里编译）：把合法的数据随机弄坏，交给每个解析外来数据的入口，要求不 panic。
//!
//! 实现流程：
//! 1. 手写的坏数据测试只能证明试过的那几种坏法没事。这里每个入口跑两万次：轮流拿一份合法样本，
//!    翻几个比特、截短、接长、中间插一段、盖掉一段、换成容易出事的字节（`mutate`），八次里有一次干脆全是随机字节。
//! 2. 随机数是固定种子的 xorshift（不加依赖）：每次跑的输入都一样，出了事能重现。
//!    入口 panic 时 `run` 接住，带着入口名、第几次和那份输入的十六进制再 panic 一次。
//! 3. 找出来的 panic 就地修，那份输入留在对应模块里当固定的回归测试。
//! 4. 要用到模块里私有函数的入口，测试写在那个模块自己的测试里，用这里的 `Rng`、`mutate`、`run`：
//!    中继消息（`transport/nostr.rs`）、历史记录的语音拆包（`history.rs`）、抖动缓冲（`voice/jitter.rs`）。
//!    语音的 Opus 解码 3.2.0 已经有同样做法的测试（`voice/mod.rs`），不重复。

use std::panic::{AssertUnwindSafe, catch_unwind};

/// 每个入口跑多少次。
pub(crate) const ROUNDS: usize = 20_000;

/// xorshift64*：固定种子，不为了好的随机性，只为了每次跑出同一串输入。
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// 0 到 n-1；n 是 0 时返回 0。
    pub(crate) fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }

    pub(crate) fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next_u64() as u8).collect()
    }
}

/// 容易出事的字节：全 0、全 1、符号位，以及 CBOR 里「后面跟 8 字节长度」的整数、字节串、文本、数组、映射、标签的头
/// 和不定长数组、映射的头（谎报长度、嵌套都从这里来）。
const NASTY: [u8; 12] = [
    0x00, 0xff, 0x7f, 0x80, 0x1b, 0x5b, 0x7b, 0x9b, 0xbb, 0xdb, 0x9f, 0xbf,
];

/// 把一份数据随机弄坏一处。
pub(crate) fn mutate(rng: &mut Rng, sample: &[u8]) -> Vec<u8> {
    let mut b = sample.to_vec();
    match rng.below(6) {
        0 => {
            for _ in 0..=rng.below(4) {
                if !b.is_empty() {
                    let at = rng.below(b.len());
                    b[at] ^= 1 << rng.below(8);
                }
            }
        }
        1 => {
            let keep = rng.below(b.len() + 1);
            b.truncate(keep);
        }
        2 => {
            let extra = rng.below(64);
            b.extend(rng.bytes(extra));
        }
        3 => {
            if !b.is_empty() {
                let at = rng.below(b.len());
                let len = rng.below(b.len() - at) + 1;
                let junk = rng.bytes(len);
                b[at..at + len].copy_from_slice(&junk);
            }
        }
        4 => {
            let at = rng.below(b.len() + 1);
            let len = rng.below(16) + 1;
            let junk = rng.bytes(len);
            b.splice(at..at, junk);
        }
        _ => {
            if !b.is_empty() {
                let at = rng.below(b.len());
                b[at] = NASTY[rng.below(NASTY.len())];
            }
        }
    }
    b
}

/// 跑 `ROUNDS` 次：轮流取样本弄坏（四次里有一次弄坏两处，八次里有一次全是随机字节），交给 `f`。
/// `f` panic 了就带着入口名、第几次和输入再 panic 一次，照着能重现。
pub(crate) fn run(name: &str, samples: &[Vec<u8>], f: impl Fn(&[u8])) {
    // 种子由入口名定：各个入口的输入互不相同，改了一个入口的样本不影响别的
    let seed = name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    let mut rng = Rng::new(seed);
    for i in 0..ROUNDS {
        let input = if samples.is_empty() || rng.below(8) == 0 {
            let len = rng.below(300);
            rng.bytes(len)
        } else {
            let once = mutate(&mut rng, &samples[i % samples.len()]);
            if rng.below(4) == 0 {
                mutate(&mut rng, &once)
            } else {
                once
            }
        };
        if catch_unwind(AssertUnwindSafe(|| f(&input))).is_err() {
            panic!(
                "随机输入测试「{name}」第 {i} 次 panic 了，输入 {} 字节：{}",
                input.len(),
                crate::crypto::hex(&input)
            );
        }
    }
}

mod tests {
    use serde_bytes::ByteBuf;
    use serde_json::{Value, json};

    use super::*;
    use crate::crypto::{Identity, RoomId, RoomKey};
    use crate::emotes::store::{
        check_anim, check_sound, check_still, parse_name, sound_mime, test_webp,
    };
    use crate::protocol::body::{self, Avatar, ProfileBody, RenameBody, RoomCreateBody};
    use crate::protocol::event::{
        FORMAT_V2, SignedEvent, UnsignedEvent, build_event, kind, verify_stateless,
    };
    use crate::protocol::frame::{
        Frame, HaveEntry, WantRange, WireEvent, decode_frame, encode_frame, short_id,
    };
    use crate::protocol::live::{LiveBody, Status, open_live, sign_live};
    use crate::protocol::packet::{open_packet, peek_room_id, seal_packets};
    use crate::protocol::room_code::{OwnerRule, RoomCode, ShortCode, TeamCode, owner_tag_of};
    use crate::rooms::state::derive_room_state;
    use crate::settings2::{Settings, normalize};
    use crate::sync::plan::{Holdings, dominates, missing_total, state_gaps};

    const ROOM: RoomId = [7; 16];
    const KEY: RoomKey = [9; 32];
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest of a tiny picture";

    fn identity(n: u8) -> Identity {
        Identity::from_secret(&[n; 32])
    }

    /// 每种实时帧内容一份。
    fn live_bodies() -> Vec<LiveBody> {
        vec![
            LiveBody::Presence {
                status: Status::Online,
                laning: false,
                version: "3.3.0".into(),
                platform: "win".into(),
                emotes: vec![1; 32],
                off: 0,
            },
            LiveBody::Ping {
                display: 1,
                x: 100,
                y: 200,
                id: "danger".into(),
            },
            LiveBody::Emote {
                display: 1,
                x: 100,
                y: 200,
                emote: "facepalm".into(),
                fx: 2,
            },
            LiveBody::Want {
                to: identity(2).member_id(),
                hash: [3; 16],
                need: vec![0, 1, 2],
            },
            LiveBody::Chunk {
                hash: [3; 16],
                i: 1,
                n: 4,
                bytes: vec![5; 200],
            },
            LiveBody::Text {
                display: 1,
                x: 100,
                y: 200,
                text: "上路来人了".into(),
            },
            LiveBody::Vote {
                id: 7,
                kind: 3,
                target: Some(identity(2).member_id()),
            },
            LiveBody::Ballot { id: 7, yes: true },
            LiveBody::Bye,
            LiveBody::Voice {
                stream: 9,
                first: 20,
                frames: vec![ByteBuf::from(vec![1u8; 40]); 3],
                end: true,
            },
        ]
    }

    fn profile(avatar: Avatar) -> Vec<u8> {
        body::encode(&ProfileBody {
            nickname: "阿狸".into(),
            avatar: Some(avatar),
        })
    }

    fn room_create() -> Vec<u8> {
        body::encode(&RoomCreateBody {
            title: "开黑".into(),
        })
    }

    /// 每种帧一份：事件、Have、Want，和装着每种内容的实时帧。
    fn frames() -> Vec<Frame> {
        let me = identity(1);
        let short = short_id(&me.member_id());
        let events = [
            build_event(&me, ROOM, 1, 1_000, kind::ROOM_CREATE, room_create()),
            build_event(
                &me,
                ROOM,
                2,
                1_001,
                kind::PROFILE,
                profile(Avatar::Preset { icon: 3, color: 4 }),
            ),
        ];
        let mut out = vec![
            Frame::Events {
                live: true,
                events: events.iter().map(WireEvent::from_signed).collect(),
            },
            Frame::Have {
                from: me.member_id(),
                period_s: 5,
                vector: vec![HaveEntry {
                    author: short,
                    max: 2,
                }],
            },
            Frame::Want {
                to: short,
                ranges: vec![WantRange {
                    author: short,
                    from_seq: 1,
                    to_seq: 2,
                }],
            },
        ];
        out.extend(
            live_bodies().iter().enumerate().map(|(i, b)| {
                Frame::Live(sign_live(&me, &ROOM, i as u64 + 1, 1_700_000_000_000, b))
            }),
        );
        out
    }

    #[test]
    fn broken_packets_never_panic() {
        let samples: Vec<Vec<u8>> = frames()
            .iter()
            .flat_map(|f| seal_packets(&KEY, &ROOM, f).unwrap())
            .collect();
        run("加密包", &samples, |b| {
            let _ = peek_room_id(b);
            let _ = open_packet(&KEY, b);
        });
    }

    /// 包解密以后的内容：队里的人可以发任何明文，所以帧的解码和它后面的每一步都要经得起坏数据。
    #[test]
    fn broken_frames_never_panic() {
        let samples: Vec<Vec<u8>> = frames().iter().map(encode_frame).collect();
        let mut mine = Holdings::default();
        mine.add(short_id(&identity(1).member_id()), 1);
        mine.add(short_id(&identity(1).member_id()), 3);
        // 验签在调试版里一次一两毫秒，解得出来的帧每个都验的话这一个测试要跑半分钟：
        // 每 16 个验一次（验签库对坏公钥、坏签名的反应也要碰到），其余的只走我们自己的代码
        let seen = std::cell::Cell::new(0usize);
        run("帧", &samples, |b| match decode_frame(b) {
            Some(Frame::Events { events, .. }) => {
                for w in events {
                    seen.set(seen.get() + 1);
                    let e = w.into_signed(ROOM);
                    if seen.get().is_multiple_of(16) {
                        let _ = verify_stateless(&e, &ROOM);
                    } else {
                        let _ = e.event.compute_id();
                    }
                }
            }
            Some(Frame::Live(w)) => {
                seen.set(seen.get() + 1);
                if seen.get().is_multiple_of(16) {
                    let _ = open_live(&w, &ROOM);
                } else {
                    let _ = body::decode::<LiveBody>(&w.3);
                }
            }
            Some(Frame::Have { vector, .. }) => {
                let _ = state_gaps(&mine, &vector);
                let _ = missing_total(&mine, &vector);
                let _ = dominates(&vector, &mine.vector());
            }
            Some(Frame::Want { .. }) | None => {}
        });
    }

    #[test]
    fn broken_bodies_never_panic() {
        let mut samples: Vec<Vec<u8>> = live_bodies().iter().map(body::encode).collect();
        samples.push(profile(Avatar::Preset { icon: 3, color: 4 }));
        samples.push(profile(Avatar::Image(PNG.to_vec())));
        samples.push(room_create());
        run("事件和实时帧的内容", &samples, |b| {
            let _ = body::decode::<LiveBody>(b);
            if let Ok(p) = body::decode::<ProfileBody>(b) {
                let _ = p.has_content();
                let _ = p.avatar.as_ref().map(Avatar::is_valid);
            }
            let _ = body::decode::<RoomCreateBody>(b);
            let _ = body::decode::<RenameBody>(b);
        });
    }

    /// 队里的人能签出任何内容的事件。推导队伍状态对任何一堆事件都要有结果，不能 panic。
    #[test]
    fn room_state_is_derived_from_any_set_of_events() {
        let authors = [1u8, 2, 3].map(|n| identity(n).member_id());
        let tag = owner_tag_of(&authors[0]);
        let bodies = [
            room_create(),
            body::encode(&RenameBody {
                title: "改个名".into(),
            }),
            profile(Avatar::Preset { icon: 3, color: 4 }),
            profile(Avatar::Image(PNG.to_vec())),
            Vec::new(),
        ];
        let kinds = [
            kind::ROOM_CREATE,
            kind::JOIN,
            kind::LEAVE,
            kind::PROFILE,
            kind::RENAME,
            kind::DISSOLVE,
            6,
            9,
            100,
            u16::MAX,
        ];
        let seqs = [0, 1, 1, 2, 2, 3, 100_000, u64::MAX];
        let mut rng = Rng::new(0x3300);
        for round in 0..3_000 {
            let events: Vec<SignedEvent> = (0..rng.below(12) + 1)
                .map(|_| {
                    let body = match rng.below(3) {
                        0 => {
                            let len = rng.below(64);
                            rng.bytes(len)
                        }
                        1 => {
                            let which = rng.below(bodies.len());
                            mutate(&mut rng, &bodies[which])
                        }
                        _ => bodies[rng.below(bodies.len())].clone(),
                    };
                    let event = UnsignedEvent {
                        format: FORMAT_V2,
                        room_id: ROOM,
                        author_id: authors[rng.below(authors.len())],
                        seq: seqs[rng.below(seqs.len())],
                        ts_ms: [0, 1_700_000_000_000, u64::MAX][rng.below(3)],
                        kind: kinds[rng.below(kinds.len())],
                        body,
                    };
                    let event_id = event.compute_id();
                    // 推导不验签（验签在入库之前），签名随便填
                    SignedEvent {
                        event,
                        event_id,
                        sig: [0; 64],
                    }
                })
                .collect();
            let done = catch_unwind(AssertUnwindSafe(|| derive_room_state(&tag, &events)));
            assert!(done.is_ok(), "第 {round} 轮推导时 panic 了：{events:?}");
        }
    }

    #[test]
    fn broken_team_codes_never_panic() {
        let code = RoomCode {
            room_secret: [5; 16],
            owner_tag: [6; 10],
        }
        .format();
        let short = ShortCode::new(&identity(1).member_id(), &[7; 16]).format();
        run("队伍码", &[code.into_bytes(), short.into_bytes()], |b| {
            let s = String::from_utf8_lossy(b);
            let _ = RoomCode::parse(&s);
            // 两种码都认的入口；解析得出来的，再生成一遍要能原样解析回来
            if let Ok(code) = TeamCode::parse(&s) {
                assert_eq!(TeamCode::parse(&code.format()), Ok(code));
                assert_eq!(TeamCode::from_bytes(&code.to_bytes()), Some(code));
            }
            let _ = TeamCode::from_bytes(b);
            let _ = OwnerRule::from_bytes(b);
            let _ = crate::base32::decode(&s, 218);
            let _ = crate::base32::decode(&s, 100);
            let _ = crate::base32::decode(&s, 64);
        });
    }

    /// 导入的表情和队友传来的静态图：只看文件头的那几个检查。
    #[test]
    fn broken_emote_files_never_panic() {
        let mut lossy = b"RIFF\0\0\0\0WEBPVP8 \0\0\0\0\0\0\0\x9d\x01\x2a".to_vec();
        lossy.extend_from_slice(&[160, 0, 160, 0]);
        let mut extended = b"RIFF\0\0\0\0WEBPVP8X\0\0\0\0\x02\0\0\0".to_vec();
        extended.extend_from_slice(&[159, 0, 0, 159, 0, 0]);
        let samples = vec![
            test_webp(160, 160, 64),
            test_webp(1024, 1, 40),
            lossy,
            extended,
            b"GIF89a\xa0\x00\xa0\x00rest".to_vec(),
            b"OggS\0\x02rest".to_vec(),
            b"RIFF\0\0\0\0WAVEfmt ".to_vec(),
            b"ID3\x04\0rest".to_vec(),
            vec![0xff, 0xfb, 0x90, 0x00],
        ];
        run("表情文件", &samples, |b| {
            let _ = check_still(b);
            let _ = check_anim(b);
            let _ = check_sound(b);
            let _ = sound_mime(b);
            let _ = parse_name(&String::from_utf8_lossy(b));
        });
    }

    fn junk(rng: &mut Rng) -> Value {
        match rng.below(10) {
            0 => Value::Null,
            1 => json!(rng.below(2) == 0),
            2 => json!(-1),
            3 => json!(u64::MAX),
            4 => json!(1e300),
            5 => json!(""),
            6 => json!("字".repeat(rng.below(300))),
            7 => json!([]),
            8 => json!({}),
            _ => json!([[[[rng.below(1000)]]], {"vk": rng.below(400)}]),
        }
    }

    /// 在 JSON 里随机挑一处换成不该出现的东西（字节层面弄坏的 JSON 大多解析不了，测不到设置的校验）。
    fn spoil(rng: &mut Rng, v: &mut Value) {
        match v {
            Value::Object(m) if !m.is_empty() && rng.below(4) != 0 => {
                let key = m.keys().nth(rng.below(m.len())).cloned();
                if let Some(inner) = key.and_then(|k| m.get_mut(&k)) {
                    spoil(rng, inner);
                }
            }
            Value::Array(a) if !a.is_empty() && rng.below(4) != 0 => {
                let at = rng.below(a.len());
                spoil(rng, &mut a[at]);
            }
            other => *other = junk(rng),
        }
    }

    /// 设置：手改坏了的、旧版本留下的都要能变成合法设置，而且规整过的再规整一次不变（存下的永远合法）。
    #[test]
    fn spoiled_settings_normalize_to_something_stable() {
        let base = serde_json::to_value(Settings::default()).unwrap();
        let mut rng = Rng::new(0x5e77);
        for round in 0..5_000 {
            let mut v = base.clone();
            for _ in 0..=rng.below(5) {
                spoil(&mut rng, &mut v);
            }
            let once = normalize(&v);
            let twice = normalize(&serde_json::to_value(&once).unwrap());
            assert_eq!(once, twice, "第 {round} 轮，输入：{v}");
        }
    }

    /// 队友要事件时报的序号区间是他说了算的：最大的数也只是查不到，不能让查库 panic。
    #[test]
    fn huge_want_ranges_only_find_nothing() {
        let conn = crate::storage::open_in_memory().unwrap();
        let author = identity(1).member_id();
        for (from, to) in [
            (0, u64::MAX),
            (u64::MAX, 0),
            (u64::MAX, u64::MAX),
            (1, 1 << 63),
        ] {
            // 超出数据库整数范围的序号是「查询出错」，别的是「查到空」；两种都行，只要不 panic
            if let Ok(rows) = crate::storage::events::query_range(&conn, &ROOM, &author, from, to) {
                assert!(rows.is_empty());
            }
        }
    }
}
