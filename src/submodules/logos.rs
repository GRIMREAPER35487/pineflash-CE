//! Boot logo gallery: downloads IronOS-Meta's logo pack for the selected iron, decodes each
//! logo into its 96x16 frames and draws them as little OLED screens to pick from.
//!
//! Logo data format (IronOS-Meta "Bootup Logos/img2logo.py"):
//!   [0xAA][frame delay / 5 ms] then frames, each one of
//!     [0xFF][192 bytes]          full frame
//!     [0xFE]                     same as the previous frame
//!     [n][n bytes: index,value]  delta from the previous frame (n < 192)
//!     [0x00]                     end of animation (padding)
//!   A frame is 96 columns x 2 pages; byte i is column i % 96, bit y is row y (+8 for i >= 96).
//!   A page that doesn't start with 0xAA (the "erase" image is all 0xFF) means "no logo".

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

use eframe::egui::{self, Color32, ColorImage, TextureHandle, TextureOptions};

use super::bl70x::parse_dfu_raw;

pub const WIDTH: usize = 96;
pub const HEIGHT: usize = 16;
const FRAME_BYTES: usize = WIDTH * HEIGHT / 8;

pub struct Logo {
    /// Display name, e.g. "ad maiora".
    pub name: String,
    pub path: PathBuf,
    /// The same logo turned for left-handed use (`_L` files), if there is one.
    pub left_path: Option<PathBuf>,
    /// Decoded frames; empty for the "remove logo" image.
    pub frames: Vec<[u8; FRAME_BYTES]>,
    /// Milliseconds per frame (0 = still image).
    pub frame_ms: u32,
}

impl Logo {
    pub fn is_erase(&self) -> bool {
        self.frames.is_empty()
    }
}

#[derive(Default)]
pub struct LogoState {
    pub logos: Vec<Logo>,
    /// Which iron (`int_name`) the list was loaded for.
    pub loaded_for: String,
    pub download: Option<Receiver<Result<usize, String>>>,
    pub filter: String,
    pub left_handed: bool,
    pub textures: HashMap<(PathBuf, usize), TextureHandle>,
}

/// Turn a logo page into frames. None means "no logo stored" (e.g. the erase image).
pub fn decode(d: &[u8]) -> Option<(Vec<[u8; FRAME_BYTES]>, u32)> {
    if d.len() < 3 || d[0] != 0xAA {
        return None;
    }
    let frame_ms = d[1] as u32 * 5;
    let mut frames = Vec::new();
    let mut cur = [0u8; FRAME_BYTES];
    let mut i = 2;
    while i < d.len() {
        let marker = d[i];
        i += 1;
        match marker {
            0x00 => break,
            0xFF => {
                if i + FRAME_BYTES > d.len() {
                    break;
                }
                cur.copy_from_slice(&d[i..i + FRAME_BYTES]);
                i += FRAME_BYTES;
            }
            0xFE => {}
            n => {
                let n = n as usize;
                if i + n > d.len() {
                    break;
                }
                for pair in d[i..i + n].chunks_exact(2) {
                    if let Some(b) = cur.get_mut(pair[0] as usize) {
                        *b = pair[1];
                    }
                }
                i += n;
            }
        }
        frames.push(cur);
        if frame_ms == 0 {
            break; // still image: one frame
        }
    }
    if frames.is_empty() {
        None
    } else {
        Some((frames, frame_ms))
    }
}

pub fn pixel(frame: &[u8; FRAME_BYTES], x: usize, y: usize) -> bool {
    let idx = x + if y >= 8 { WIDTH } else { 0 };
    frame[idx] >> (y % 8) & 1 == 1
}

fn pack_name(int_name: &str) -> &'static str {
    if int_name == "Pinecil" {
        "pinecilv1"
    } else {
        "pinecilv2"
    }
}

/// ~/PineFlash/logos/<pinecilv1|pinecilv2>
pub fn logos_dir(int_name: &str) -> PathBuf {
    simple_home_dir::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("PineFlash")
        .join("logos")
        .join(pack_name(int_name))
}

fn display_name(file: &str) -> String {
    let mut s = file.trim_end_matches(".dfu").to_string();
    for ext in [".png", ".gif", ".bmp", ".jpg"] {
        if let Some(stripped) = s.strip_suffix(ext) {
            s = stripped.to_string();
        }
    }
    s.replace(['_', '-'], " ")
}

/// Read every .dfu logo in `dir`; `_L` files are attached to their right-handed twin.
pub fn load(dir: &Path) -> Vec<Logo> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e.eq_ignore_ascii_case("dfu")).unwrap_or(false))
        .collect();
    files.sort();

    let stem = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let left_of = |name: &str| -> Option<String> {
        // "x.png.dfu" -> "x_L.png.dfu"; "erase_stored_image.dfu" -> "erase_stored_image_L.dfu"
        let base = name.trim_end_matches(".dfu");
        let (head, ext) = match base.rfind('.') {
            Some(i) => (&base[..i], &base[i..]),
            None => (base, ""),
        };
        Some(format!("{}_L{}.dfu", head, ext))
    };

    let names: Vec<String> = files.iter().map(|p| stem(p)).collect();
    let mut logos = Vec::new();
    for (path, name) in files.iter().zip(&names) {
        let base = name.trim_end_matches(".dfu");
        let head = base.rsplit_once('.').map(|(h, _)| h).unwrap_or(base);
        if head.ends_with("_L") {
            continue; // shown through its twin
        }
        let Ok(bytes) = std::fs::read(path) else { continue };
        let Ok((_, data)) = parse_dfu_raw(&bytes) else { continue };
        let (frames, frame_ms) = decode(&data).unwrap_or_default();
        let left_path = left_of(name)
            .filter(|l| names.contains(l))
            .map(|l| dir.join(l));
        let display = if frames.is_empty() { "No logo (remove)".to_string() } else { display_name(name) };
        logos.push(Logo { name: display, path: path.clone(), left_path, frames, frame_ms });
    }
    // The remove option first, then alphabetical.
    logos.sort_by(|a, b| b.is_erase().cmp(&a.is_erase()).then(a.name.cmp(&b.name)));
    logos
}

/// Download and unpack IronOS-Meta's logo pack (runs on a background thread).
pub fn download_pack(int_name: &str) -> Result<usize, String> {
    let pack = pack_name(int_name);
    let url = format!("https://github.com/Ralim/IronOS-Meta/releases/download/latest/{}.zip", pack);
    let zip = crate::download(&url).map_err(|e| format!("could not download the logo pack: {}", e))?;

    let tmp = std::env::temp_dir().join(format!("pineflash-logos-{}", pack));
    let _ = std::fs::remove_dir_all(&tmp);
    zip_extract::extract(std::io::Cursor::new(zip), &tmp, true)
        .map_err(|e| format!("the logo pack is not a valid zip: {}", e))?;

    let dir = logos_dir(int_name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {}", dir.display(), e))?;
    let mut count = 0;
    let mut stack = vec![tmp.clone()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).map_err(|e| e.to_string())?.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().map(|e| e.eq_ignore_ascii_case("dfu")).unwrap_or(false) {
                // Only keep files that really are logo DFUs.
                let ok = std::fs::read(&p).ok().and_then(|b| parse_dfu_raw(&b).ok()).is_some();
                if ok {
                    if let Some(name) = p.file_name() {
                        std::fs::copy(&p, dir.join(name)).map_err(|e| e.to_string())?;
                        count += 1;
                    }
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    if count == 0 {
        return Err("the logo pack had no logo files".into());
    }
    Ok(count)
}

/// Texture for one frame, drawn like the iron's OLED: light pixels on black.
pub fn frame_texture(
    ctx: &egui::Context,
    cache: &mut HashMap<(PathBuf, usize), TextureHandle>,
    logo: &Logo,
    index: usize,
) -> Option<TextureHandle> {
    let frame = logo.frames.get(index)?;
    let key = (logo.path.clone(), index);
    if let Some(t) = cache.get(&key) {
        return Some(t.clone());
    }
    let on = Color32::from_rgb(232, 240, 255);
    let off = Color32::from_rgb(10, 10, 12);
    let mut img = ColorImage::new([WIDTH, HEIGHT], off);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            if pixel(frame, x, y) {
                img.pixels[y * WIDTH + x] = on;
            }
        }
    }
    let tex = ctx.load_texture(format!("logo-{}-{}", logo.path.display(), index), img, TextureOptions::NEAREST);
    cache.insert(key, tex.clone());
    Some(tex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_still_full_frame() {
        let mut d = vec![0xAA, 0x00, 0xFF];
        let mut f = [0u8; FRAME_BYTES];
        f[0] = 0b0000_0001; // (0,0)
        f[WIDTH + 5] = 0b1000_0000; // (5,15)
        d.extend_from_slice(&f);
        d.resize(1024, 0);
        let (frames, ms) = decode(&d).unwrap();
        assert_eq!((frames.len(), ms), (1, 0));
        assert!(pixel(&frames[0], 0, 0));
        assert!(pixel(&frames[0], 5, 15));
        assert!(!pixel(&frames[0], 1, 0));
    }

    #[test]
    fn decode_animation_with_deltas() {
        let mut d = vec![0xAA, 20, 0xFF];
        d.extend_from_slice(&[0u8; FRAME_BYTES]);
        d.extend_from_slice(&[4, 3, 0x01, 10, 0x02]); // delta: byte3=1, byte10=2
        d.push(0xFE); // repeat
        d.resize(1024, 0);
        let (frames, ms) = decode(&d).unwrap();
        assert_eq!((frames.len(), ms), (3, 100));
        assert_eq!(frames[1][3], 1);
        assert_eq!(frames[1][10], 2);
        assert_eq!(frames[2], frames[1]);
    }

    #[test]
    fn erase_image_is_no_logo() {
        assert!(decode(&[0xFF; 1024]).is_none());
    }

    #[test]
    fn names() {
        assert_eq!(display_name("ad_maiora.png.dfu"), "ad maiora");
        assert_eq!(display_name("arcade-galaga.gif.dfu"), "arcade galaga");
    }

    /// Decode every logo in a local pack folder, if given.
    #[test]
    fn real_pack_if_present() {
        let Ok(dir) = std::env::var("PINECIL_LOGO_DIR") else { return };
        let logos = load(Path::new(&dir));
        assert!(logos.len() >= 30, "only {} logos", logos.len());
        assert!(logos[0].is_erase());
        assert!(logos.iter().skip(1).all(|l| !l.frames.is_empty()));
        assert!(logos.iter().all(|l| l.left_path.is_some()));
        println!(
            "{} logos, {} animated",
            logos.len(),
            logos.iter().filter(|l| l.frames.len() > 1).count()
        );
    }
}
