//! 表情文件：导入的表情和收到的队友表情。检查规则移植自 lolPing `src/main/emoteLibrary.ts`、`emoteCache.ts`
//! （MIT，© 2026 Danrui Meng）。
//!
//! 实现流程：
//! 1. 导入的表情放在数据目录的 `emotes/`：`<哈希>.webp` 是静态图（控制面板已经缩到 160 像素、12 KB 以内），
//!    `<哈希>.anim` 是原动画（GIF 或 WebP，可选），`<哈希>.sound` 是音效（OGG、MP3 或 WAV，可选）。哈希是静态图 SHA-256 的前 16 字节。
//! 2. 写进来之前按文件头检查类型、大小、边长：静态图必须是不动的 WebP，透明窗口把它当 `<img>` 画，动的会喧宾夺主。
//! 3. 队友的静态图收齐、验过哈希后放在 `emote-cache/`，最多 200 个、4 MB，超了按最近一次用到的时间删最旧的。
//! 4. 透明窗口和控制面板用 `emote://` 地址取文件（`lib.rs` 注册），这里只认 `<32 位十六进制>.<webp|anim|sound>` 这样的名字。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use crate::crypto;

pub const STILL_MAX_BYTES: usize = 12 * 1024;
pub const STILL_SIDE: u32 = 160;
pub const ANIM_MAX_BYTES: usize = 5 * 1024 * 1024;
pub const ANIM_MAX_SIDE: u32 = 1024;
pub const SOUND_MAX_BYTES: usize = 1024 * 1024;
const CACHE_MAX_FILES: usize = 200;
const CACHE_MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Still,
    Anim,
    Sound,
}

impl Part {
    fn ext(self) -> &'static str {
        match self {
            Part::Still => "webp",
            Part::Anim => "anim",
            Part::Sound => "sound",
        }
    }
}

/// `emote://` 的文件名 → 哈希和哪一部分。
pub fn parse_name(name: &str) -> Option<(&str, Part)> {
    let (hash, ext) = name.split_once('.')?;
    let part = match ext {
        "webp" => Part::Still,
        "anim" => Part::Anim,
        "sound" => Part::Sound,
        _ => return None,
    };
    super::is_hash(hash).then_some((hash, part))
}

/// 静态图的哈希：SHA-256 的前 16 字节，十六进制。
pub fn hash_of(still: &[u8]) -> String {
    crate::crypto::hex(&crypto::sha256(&[still])[..16])
}

fn u16_le(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes(
        b.get(at..at + 2)?.try_into().ok()?,
    )))
}

fn u24_le(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 3)?;
    Some(u32::from(s[0]) | u32::from(s[1]) << 8 | u32::from(s[2]) << 16)
}

fn is_webp(b: &[u8]) -> bool {
    b.len() >= 16 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP"
}

fn is_gif(b: &[u8]) -> bool {
    b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a")
}

/// WebP 的宽高（VP8、VP8L、VP8X 三种头）。
fn webp_size(b: &[u8]) -> Option<(u32, u32)> {
    if !is_webp(b) {
        return None;
    }
    match b.get(12..16)? {
        b"VP8 " => {
            (b.get(23..26)? == [0x9d, 0x01, 0x2a]).then_some(())?;
            Some((u16_le(b, 26)? & 0x3fff, u16_le(b, 28)? & 0x3fff))
        }
        b"VP8L" => {
            (*b.get(20)? == 0x2f).then_some(())?;
            let bits = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
            Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
        }
        b"VP8X" => Some((u24_le(b, 24)? + 1, u24_le(b, 27)? + 1)),
        _ => None,
    }
}

fn webp_animated(b: &[u8]) -> bool {
    b.get(12..16) == Some(b"VP8X") && b.get(20).is_some_and(|f| f & 0x02 != 0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportError {
    /// 不是要的类型（或是动的静态图、边长超了）。
    Type,
    Size,
    /// 已经有 24 个了。
    Full,
    Disk,
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ImportError::Type => crate::tr!("文件类型不对", "Wrong file type"),
            ImportError::Size => crate::tr!("文件太大", "File too large"),
            ImportError::Full => {
                crate::tr!("最多导入 24 个表情", "You can import at most 24 emotes")
            }
            ImportError::Disk => crate::tr!("写不进数据目录", "Can't write to the data folder"),
        })
    }
}

/// 静态图：不动的 WebP，12 KB、160 像素以内。队友发来的静态图也过这一关。
pub fn check_still(b: &[u8]) -> Result<(), ImportError> {
    if !is_webp(b) || webp_animated(b) {
        return Err(ImportError::Type);
    }
    if b.len() > STILL_MAX_BYTES {
        return Err(ImportError::Size);
    }
    match webp_size(b) {
        Some((w, h)) if w.max(h) <= STILL_SIDE => Ok(()),
        _ => Err(ImportError::Type),
    }
}

/// 动画：GIF 或 WebP，5 MB、1024 像素以内。
pub fn check_anim(b: &[u8]) -> Result<(), ImportError> {
    let size = if is_gif(b) {
        u16_le(b, 6).zip(u16_le(b, 8))
    } else {
        webp_size(b)
    };
    let Some((w, h)) = size else {
        return Err(ImportError::Type);
    };
    if b.len() > ANIM_MAX_BYTES {
        return Err(ImportError::Size);
    }
    if w.max(h) > ANIM_MAX_SIDE {
        return Err(ImportError::Type);
    }
    Ok(())
}

/// 音效的类型：OGG、WAV、MP3，1 MB 以内（5 秒的限制由控制面板解码后检查）。
pub fn sound_mime(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"OggS") {
        Some("audio/ogg")
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WAVE" {
        Some("audio/wav")
    } else if b.starts_with(b"ID3") || (b.len() >= 2 && b[0] == 0xff && b[1] & 0xe0 == 0xe0) {
        Some("audio/mpeg")
    } else {
        None
    }
}

pub fn check_sound(b: &[u8]) -> Result<(), ImportError> {
    sound_mime(b).ok_or(ImportError::Type)?;
    if b.len() > SOUND_MAX_BYTES {
        return Err(ImportError::Size);
    }
    Ok(())
}

/// 收到的队友静态图：文件名、大小、最近一次用到的时间。
struct Cache {
    dir: PathBuf,
    entries: HashMap<String, (u64, SystemTime)>,
}

impl Cache {
    fn open(dir: PathBuf) -> Self {
        let mut entries = HashMap::new();
        if let Ok(list) = std::fs::read_dir(&dir) {
            for e in list.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                let Some((hash, Part::Still)) = parse_name(&name) else {
                    continue;
                };
                if let Ok(m) = e.metadata() {
                    let at = m.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    entries.insert(hash.to_string(), (m.len(), at));
                }
            }
        }
        Self { dir, entries }
    }

    fn path(&self, hash: &str) -> PathBuf {
        self.dir.join(format!("{hash}.webp"))
    }

    fn put(&mut self, hash: &str, bytes: &[u8]) -> bool {
        if std::fs::create_dir_all(&self.dir).is_err()
            || std::fs::write(self.path(hash), bytes).is_err()
        {
            return false;
        }
        self.entries
            .insert(hash.to_string(), (bytes.len() as u64, SystemTime::now()));
        self.evict();
        true
    }

    fn touch(&mut self, hash: &str) {
        if let Some(e) = self.entries.get_mut(hash) {
            e.1 = SystemTime::now();
        }
    }

    fn evict(&mut self) {
        let mut total: u64 = self.entries.values().map(|e| e.0).sum();
        while self.entries.len() > CACHE_MAX_FILES || total > CACHE_MAX_BYTES {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.1)
                .map(|(h, _)| h.clone())
            else {
                break;
            };
            let _ = std::fs::remove_file(self.path(&oldest));
            total -= self.entries.remove(&oldest).map_or(0, |e| e.0);
        }
    }
}

/// 表情文件的总管，透明窗口线程、传输线程、命令都拿同一个。
pub struct Store {
    library: PathBuf,
    cache: Mutex<Cache>,
}

impl Store {
    /// `dir` 是数据目录。
    pub fn open(dir: &Path) -> Self {
        Self {
            library: dir.join("emotes"),
            cache: Mutex::new(Cache::open(dir.join("emote-cache"))),
        }
    }

    fn path(&self, hash: &str, part: Part) -> PathBuf {
        self.library.join(format!("{hash}.{}", part.ext()))
    }

    /// 检查后写进 `emotes/`，返回哈希。同一张静态图再导入一次会换掉它的动画和音效。
    pub fn import(
        &self,
        still: &[u8],
        anim: Option<&[u8]>,
        sound: Option<&[u8]>,
    ) -> Result<String, ImportError> {
        check_still(still)?;
        if let Some(a) = anim {
            check_anim(a)?;
        }
        if let Some(s) = sound {
            check_sound(s)?;
        }
        let hash = hash_of(still);
        std::fs::create_dir_all(&self.library).map_err(|_| ImportError::Disk)?;
        let write = |part: Part, b: Option<&[u8]>| match b {
            Some(b) => std::fs::write(self.path(&hash, part), b),
            None => match std::fs::remove_file(self.path(&hash, part)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            },
        };
        write(Part::Still, Some(still))
            .and_then(|()| write(Part::Anim, anim))
            .and_then(|()| write(Part::Sound, sound))
            .map_err(|_| ImportError::Disk)?;
        Ok(hash)
    }

    pub fn remove(&self, hash: &str) {
        for part in [Part::Still, Part::Anim, Part::Sound] {
            let _ = std::fs::remove_file(self.path(hash, part));
        }
    }

    /// 自己导入的静态图（发给要它的队友）。
    pub fn own_still(&self, hash: &str) -> Option<Vec<u8>> {
        std::fs::read(self.path(hash, Part::Still)).ok()
    }

    /// 这张静态图本机已经有了：自己导入的，或者收到过。
    pub fn has_still(&self, hash: &str) -> bool {
        self.path(hash, Part::Still).exists() || self.lock().entries.contains_key(hash)
    }

    /// 收到的队友静态图，已经验过哈希和文件头。
    pub fn put_cached(&self, hash: &str, bytes: &[u8]) -> bool {
        self.lock().put(hash, bytes)
    }

    /// `emote://` 要的文件和它的类型。动画没有就给静态图；导入的找不到再找收到的。
    pub fn read(&self, name: &str) -> Option<(Vec<u8>, &'static str)> {
        let (hash, part) = parse_name(name)?;
        let own = |p: Part| std::fs::read(self.path(hash, p)).ok();
        match part {
            Part::Sound => own(Part::Sound).and_then(|b| sound_mime(&b).map(|m| (b, m))),
            Part::Anim => {
                let b = own(Part::Anim).or_else(|| own(Part::Still))?;
                Some((
                    b.clone(),
                    if is_gif(&b) {
                        "image/gif"
                    } else {
                        "image/webp"
                    },
                ))
            }
            Part::Still => {
                if let Some(b) = own(Part::Still) {
                    return Some((b, "image/webp"));
                }
                let mut cache = self.lock();
                let b = std::fs::read(cache.path(hash)).ok()?;
                cache.touch(hash);
                Some((b, "image/webp"))
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.cache.lock().expect("表情缓存锁没有中毒")
    }
}

/// 测试用：一张 `w`×`h` 的 VP8L（无损）WebP 的头，后面补 0 到 `len` 字节。
#[cfg(test)]
pub fn test_webp(w: u32, h: u32, len: usize) -> Vec<u8> {
    let mut b = b"RIFF\0\0\0\0WEBPVP8L\0\0\0\0\x2f".to_vec();
    b.extend_from_slice(&((w - 1) | (h - 1) << 14).to_le_bytes());
    b.resize(len.max(b.len()), 0);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::test_webp as webp;

    fn dir() -> PathBuf {
        let tag = crypto::random_bytes::<8>().map(u64::from_le_bytes).unwrap();
        let d = std::env::temp_dir().join(format!("gang-emotes-{tag}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn names_are_hash_and_part_only() {
        let h = "0123456789abcdef0123456789abcdef";
        assert_eq!(parse_name(&format!("{h}.webp")), Some((h, Part::Still)));
        assert_eq!(parse_name(&format!("{h}.sound")), Some((h, Part::Sound)));
        for bad in [
            format!("{h}.exe"),
            format!("../{h}.webp"),
            "x.webp".into(),
            h.to_string(),
        ] {
            assert_eq!(parse_name(&bad), None, "{bad}");
        }
    }

    #[test]
    fn stills_must_be_small_still_webps() {
        assert_eq!(check_still(&webp(160, 120, 2000)), Ok(()));
        assert_eq!(check_still(&webp(161, 10, 2000)), Err(ImportError::Type));
        assert_eq!(
            check_still(&webp(100, 100, STILL_MAX_BYTES + 1)),
            Err(ImportError::Size)
        );
        assert_eq!(
            check_still(b"GIF89a\x10\x00\x10\x00"),
            Err(ImportError::Type)
        );
        // VP8X 头带动画标记
        let mut anim = b"RIFF\0\0\0\0WEBPVP8X\0\0\0\0\x02\0\0\0".to_vec();
        anim.extend_from_slice(&[99, 0, 0, 99, 0, 0]);
        assert_eq!(webp_size(&anim), Some((100, 100)));
        assert_eq!(check_still(&anim), Err(ImportError::Type));
        assert_eq!(check_anim(&anim), Ok(()));
    }

    #[test]
    fn animations_and_sounds_are_checked_by_header() {
        let gif = b"GIF89a\x00\x04\x00\x04rest".to_vec(); // 1024×1024
        assert_eq!(check_anim(&gif), Ok(()));
        assert_eq!(
            check_anim(b"GIF89a\x01\x04\x00\x01"),
            Err(ImportError::Type)
        ); // 1025 宽
        assert_eq!(check_anim(b"not an image"), Err(ImportError::Type));
        assert_eq!(sound_mime(b"OggS...."), Some("audio/ogg"));
        assert_eq!(sound_mime(b"ID3\x04"), Some("audio/mpeg"));
        assert_eq!(sound_mime(b"RIFF\0\0\0\0WAVEfmt "), Some("audio/wav"));
        assert_eq!(check_sound(b"<html>"), Err(ImportError::Type));
        let mut big = b"OggS".to_vec();
        big.resize(SOUND_MAX_BYTES + 1, 0);
        assert_eq!(check_sound(&big), Err(ImportError::Size));
    }

    /// 队友发来的静态图只看文件头：截断在文件头任何位置的图都被拒，读头时不越界。
    #[test]
    fn truncated_headers_are_rejected() {
        // 三种头各自刚好够读出宽高的长度，都是 64×64
        let mut vp8 = b"RIFF\0\0\0\0WEBPVP8 \0\0\0\0\0\0\0\x9d\x01\x2a".to_vec();
        vp8.extend_from_slice(&[64, 0, 64, 0]);
        let mut vp8x = b"RIFF\0\0\0\0WEBPVP8X\0\0\0\0\0\0\0\0".to_vec();
        vp8x.extend_from_slice(&[63, 0, 0, 63, 0, 0]);
        let vp8l = webp(64, 64, 0);
        for full in [&vp8, &vp8l, &vp8x] {
            assert_eq!(check_still(full), Ok(()));
            for len in 0..full.len() {
                assert!(check_still(&full[..len]).is_err(), "截到 {len} 字节");
                let _ = check_anim(&full[..len]);
            }
        }
        for len in 0..10 {
            assert!(check_anim(&b"GIF89a\x10\x00\x10\x00"[..len]).is_err());
            let _ = sound_mime(&b"RIFF\0\0\0\0WAVE"[..len]);
        }
    }

    #[test]
    fn imports_are_stored_by_hash_and_served() {
        let d = dir();
        let store = Store::open(&d);
        let still = webp(64, 64, 500);
        let hash = store.import(&still, None, Some(b"OggS1234")).unwrap();
        assert_eq!(hash, hash_of(&still));
        assert!(store.has_still(&hash));
        assert_eq!(
            store.read(&format!("{hash}.webp")),
            Some((still.clone(), "image/webp"))
        );
        assert_eq!(
            store.read(&format!("{hash}.anim")).map(|r| r.1),
            Some("image/webp"),
            "没有动画就给静态图"
        );
        assert_eq!(
            store.read(&format!("{hash}.sound")).map(|r| r.1),
            Some("audio/ogg")
        );
        // 再导入一次不带音效：音效去掉
        store
            .import(&still, Some(b"GIF89a\x10\x00\x10\x00"), None)
            .unwrap();
        assert_eq!(store.read(&format!("{hash}.sound")), None);
        assert_eq!(
            store.read(&format!("{hash}.anim")).map(|r| r.1),
            Some("image/gif")
        );
        store.remove(&hash);
        assert!(!store.has_still(&hash));
        assert_eq!(store.import(b"junk", None, None), Err(ImportError::Type));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn cache_keeps_received_stills_and_evicts_the_oldest() {
        let d = dir();
        let store = Store::open(&d);
        let first = webp(10, 10, 100);
        let h = hash_of(&first);
        assert!(store.put_cached(&h, &first));
        assert!(store.has_still(&h));
        assert_eq!(store.read(&format!("{h}.webp")).map(|r| r.0), Some(first));
        assert_eq!(store.read(&format!("{h}.anim")), None, "收到的只有静态图");
        // 重新打开还在
        let again = Store::open(&d);
        assert!(again.has_still(&h));
        // 超过个数上限时删最旧的
        let mut cache = again.lock();
        for i in 0..CACHE_MAX_FILES {
            cache.put(&format!("{i:032x}"), b"x");
        }
        assert_eq!(cache.entries.len(), CACHE_MAX_FILES);
        assert!(!cache.entries.contains_key(&h), "最早的那个被删了");
        drop(cache);
        let _ = std::fs::remove_dir_all(d);
    }
}
