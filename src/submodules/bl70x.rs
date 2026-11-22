//! Native flasher for the Pinecil V2 (Bouffalo BL702, "bl70x") over its USB serial bootloader.
//!
//! The protocol follows pine64/blisp (MIT): lib/blisp.c, lib/blisp_easy.c,
//! tools/blisp/src/common.c and tools/blisp/src/cmd/write.c. Differences from blisp:
//!
//! * Handshake: plain `U` sync bytes. blisp sends `BOUFFALOLAB5555RESET` immediately
//!   followed by `U`s, which some irons reject on Windows (`FL 0x0102`).
//! * Uses the `serialport` crate (native Win32 serial API) instead of libserialport.
//! * Safety: a full flash backup is saved before anything is erased; the boot header
//!   sector and the IronOS settings sector are never erased (unless the header is
//!   missing, where blisp would always rewrite it) and are checked afterwards; every
//!   written byte is read back and compared.

use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serialport::{ClearBuffer, SerialPort};

/// Bouffalo's eflash_loader, loaded into RAM only (see assets/bl70x_eflash_loader.NOTICE.txt).
const EFLASH_LOADER: &[u8] = include_bytes!("../../assets/bl70x_eflash_loader.bin");
const TCM_ADDRESS: u32 = 0x2201_0000;
pub const FIRMWARE_ADDRESS: u32 = 0x2000;
const SECTOR: u32 = 0x1000;
const BAUD: u32 = 460_800;
const READ_CHUNK: u32 = 4096;
const WRITE_CHUNK: usize = 2052; // same as blisp on Windows/Linux
const LOADER_CHUNK: usize = 4092;
const USB_RESET: &[u8] = b"BOUFFALOLAB5555RESET\0\0";

pub type FlashResult<T> = Result<T, String>;

/// Messages sent from the flashing thread to the UI.
#[derive(Debug)]
pub enum FlashEvent {
    Log(String),
    Stage(String),
    Progress(f32),
    Finished(FlashResult<String>),
}

/// What to put on the iron.
pub struct FlashJob {
    pub port: String,
    pub firmware: Vec<u8>,
    pub firmware_name: String,
    pub backup_dir: PathBuf,
}

// ---------------------------------------------------------------------------
// CRC32 (IEEE), used by the boot header and segment header
// ---------------------------------------------------------------------------
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { 0xEDB8_8320 ^ (crc >> 1) } else { crc >> 1 };
        }
    }
    !crc
}

// ---------------------------------------------------------------------------
// Boot headers (byte-exact copies of blisp's structs; CRCs are checked in tests)
// ---------------------------------------------------------------------------

/// Flash config used while booting the eflash_loader from RAM (blisp_easy_load_ram_app).
const RAM_FLASH_CFG: [u8; 84] = [
    0x04, 0x01, 0x01, 0x01, 0x66, 0x99, 0xFF, 0x03, 0x9F, 0x00, 0x9F, 0x00, 0x04, 0xEF, 0x00, 0x01,
    0xC7, 0x20, 0x52, 0xD8, 0x06, 0x02, 0x32, 0x00, 0x0B, 0x01, 0x0B, 0x01, 0x3B, 0x01, 0xBB, 0x00,
    0x6B, 0x01, 0xEB, 0x02, 0xEB, 0x02, 0x02, 0x50, 0x00, 0x01, 0x00, 0x01, 0x01, 0x00, 0x02, 0x01,
    0x01, 0x01, 0xAB, 0x01, 0x05, 0x35, 0x00, 0x00, 0x01, 0x31, 0x00, 0x00, 0x38, 0xFF, 0x20, 0xFF,
    0x77, 0x03, 0x02, 0x40, 0x77, 0x03, 0x02, 0xF0, 0x2C, 0x01, 0xB0, 0x04, 0xB0, 0x04, 0x05, 0x00,
    0x40, 0x0D, 0x03, 0x00,
];
const RAM_FLASH_CFG_CRC: u32 = 0xC4BD_D748;
const RAM_CLK_CFG: [u8; 8] = [0x04, 0x04, 0x00, 0x01, 0x02, 0x00, 0x00, 0x00];
const RAM_CLK_CFG_CRC: u32 = 0x824E_14BB;

/// Flash config written to flash offset 0 in front of the firmware (fill_up_boot_header in write.c).
const FLASH_FLASH_CFG: [u8; 84] = [
    0x11, 0x00, 0x01, 0x01, 0x66, 0x99, 0xFF, 0x03, 0x9F, 0x00, 0x9F, 0x00, 0x04, 0xC2, 0x00, 0x01,
    0xC7, 0x20, 0x52, 0xD8, 0x06, 0x02, 0x32, 0x00, 0x0B, 0x01, 0x0B, 0x01, 0x3B, 0x01, 0xBB, 0x00,
    0x6B, 0x01, 0xEB, 0x02, 0xEB, 0x02, 0x02, 0x50, 0x00, 0x01, 0x00, 0x01, 0x01, 0x00, 0x02, 0x01,
    0x02, 0x01, 0xAB, 0x01, 0x05, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x38, 0xFF, 0x00, 0xFF,
    0x77, 0x03, 0x02, 0x40, 0x77, 0x03, 0x02, 0xF0, 0x2C, 0x01, 0xB0, 0x04, 0xB0, 0x04, 0x05, 0x00,
    0xFF, 0xFF, 0x14, 0x00,
];
const FLASH_FLASH_CFG_CRC: u32 = 0xE43C_762A;
const FLASH_CLK_CFG: [u8; 8] = [0x01, 0x04, 0x00, 0x01, 0x03, 0x00, 0x00, 0x00];
const FLASH_CLK_CFG_CRC: u32 = 0x7212_7DBA;
/// Header bytes blisp never initialises (clkCfg magic and clkCfg reserved); ignored when comparing.
const HEADER_UNSET_BYTES: [std::ops::Range<usize>; 2] = [0x64..0x68, 0x6A..0x6C];

struct HeaderParts<'a> {
    clk_magic: &'a [u8; 4],
    flash_cfg: &'a [u8; 84],
    flash_cfg_crc: u32,
    clk_cfg: &'a [u8; 8],
    clk_cfg_crc: u32,
    segment_cnt: u32,
    flash_offset: u32,
    rsv1: u32,
    rsv2: u32,
}

fn build_header(p: &HeaderParts) -> [u8; 176] {
    let mut h = Vec::with_capacity(176);
    h.extend_from_slice(b"BFNP");
    h.extend_from_slice(&1u32.to_le_bytes());
    h.extend_from_slice(b"FCFG");
    h.extend_from_slice(p.flash_cfg);
    h.extend_from_slice(&p.flash_cfg_crc.to_le_bytes());
    h.extend_from_slice(p.clk_magic);
    h.extend_from_slice(p.clk_cfg);
    h.extend_from_slice(&p.clk_cfg_crc.to_le_bytes());
    h.extend_from_slice(&0x0003_0300u32.to_le_bytes()); // no_segment, cache_enable, crc_ignore, hash_ignore
    h.extend_from_slice(&p.segment_cnt.to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes()); // bootentry
    h.extend_from_slice(&p.flash_offset.to_le_bytes());
    h.extend_from_slice(&[0xEF, 0xBE, 0xAD, 0xDE]);
    h.extend_from_slice(&[0u8; 28]);
    h.extend_from_slice(&p.rsv1.to_le_bytes());
    h.extend_from_slice(&p.rsv2.to_le_bytes());
    h.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    h.try_into().expect("boot header is 176 bytes")
}

/// Header sent to the BootROM to run the eflash_loader from RAM.
pub fn ram_boot_header() -> [u8; 176] {
    build_header(&HeaderParts {
        clk_magic: b"PCFG",
        flash_cfg: &RAM_FLASH_CFG,
        flash_cfg_crc: RAM_FLASH_CFG_CRC,
        clk_cfg: &RAM_CLK_CFG,
        clk_cfg_crc: RAM_CLK_CFG_CRC,
        segment_cnt: 1,
        flash_offset: TCM_ADDRESS,
        rsv1: 0,
        rsv2: 0,
    })
}

/// Header blisp writes to flash offset 0. Unset bytes are zero, as on irons flashed by blisp.
pub fn flash_boot_header() -> [u8; 176] {
    build_header(&HeaderParts {
        clk_magic: &[0; 4],
        flash_cfg: &FLASH_FLASH_CFG,
        flash_cfg_crc: FLASH_FLASH_CFG_CRC,
        clk_cfg: &FLASH_CLK_CFG,
        clk_cfg_crc: FLASH_CLK_CFG_CRC,
        segment_cnt: 0xCDA8,
        flash_offset: FIRMWARE_ADDRESS,
        rsv1: 0x1000,
        rsv2: 0x2000,
    })
}

/// True if `current` (first 176 bytes of flash) is blisp's header, ignoring bytes blisp leaves unset.
pub fn header_matches(current: &[u8]) -> bool {
    let expected = flash_boot_header();
    current.len() >= 176
        && (0..176).all(|i| {
            HEADER_UNSET_BYTES.iter().any(|r| r.contains(&i)) || current[i] == expected[i]
        })
}

// ---------------------------------------------------------------------------
// Serial transport
// ---------------------------------------------------------------------------
pub struct Bl70x {
    port: Box<dyn SerialPort>,
    name: String,
}

fn ms(v: u64) -> Duration {
    Duration::from_millis(v)
}

impl Bl70x {
    fn open(name: &str) -> FlashResult<Self> {
        let mut port = serialport::new(name, BAUD)
            .timeout(ms(50))
            .open()
            .map_err(|e| format!("could not open {}: {}", name, e))?;
        let _ = port.write_data_terminal_ready(true);
        let _ = port.write_request_to_send(false);
        sleep(ms(50));
        let _ = port.clear(ClearBuffer::Input);
        Ok(Self { port, name: name.to_string() })
    }

    fn clear_input(&mut self) {
        let _ = self.port.clear(ClearBuffer::Input);
    }

    fn write(&mut self, data: &[u8]) -> FlashResult<()> {
        self.port
            .write_all(data)
            .and_then(|_| self.port.flush())
            .map_err(|e| format!("write to {} failed: {}", self.name, e))
    }

    /// Read exactly `n` bytes, or None if they don't arrive within `timeout_ms`.
    fn read_exact_timeout(&mut self, n: usize, timeout_ms: u64) -> FlashResult<Option<Vec<u8>>> {
        let mut buf = vec![0u8; n];
        let mut got = 0;
        let deadline = Instant::now() + ms(timeout_ms);
        while got < n && Instant::now() < deadline {
            match self.port.read(&mut buf[got..]) {
                Ok(r) => got += r,
                Err(e) if e.kind() == ErrorKind::TimedOut => {}
                Err(e) => return Err(format!("read from {} failed: {}", self.name, e)),
            }
        }
        Ok(if got == n { Some(buf) } else { None })
    }

    /// Whatever arrives within `wait_ms`.
    fn read_available(&mut self, wait_ms: u64) -> Vec<u8> {
        sleep(ms(wait_ms));
        let n = self.port.bytes_to_read().unwrap_or(0) as usize;
        let mut buf = vec![0u8; n];
        if n > 0 {
            let got = self.port.read(&mut buf).unwrap_or(0);
            buf.truncate(got);
        }
        buf
    }

    /// Frame: [cmd, checksum, len_lo, len_hi, payload...] (blisp_send_command).
    fn send_command(&mut self, cmd: u8, payload: &[u8], checksum: bool) -> FlashResult<()> {
        let len = payload.len();
        if len > u16::MAX as usize {
            return Err(format!("payload too large: {}", len));
        }
        let mut chk = 0u32;
        if checksum {
            chk = (len & 0xFF) as u32 + ((len >> 8) & 0xFF) as u32;
            chk += payload.iter().map(|&b| b as u32).sum::<u32>();
        }
        let mut frame = Vec::with_capacity(4 + len);
        frame.extend_from_slice(&[cmd, (chk & 0xFF) as u8, (len & 0xFF) as u8, (len >> 8) as u8]);
        frame.extend_from_slice(payload);
        self.write(&frame)
    }

    /// "OK" [+ len + data] / "PD" (pending, keep waiting) / "FL" + error code (blisp_receive_response).
    fn response(&mut self, expect_payload: bool, read_ms: u64, overall_ms: u64) -> FlashResult<Vec<u8>> {
        let start = Instant::now();
        loop {
            if start.elapsed() > ms(overall_ms) {
                return Err(format!("no final response from the iron within {} ms", overall_ms));
            }
            let head = self
                .read_exact_timeout(2, read_ms)?
                .ok_or_else(|| "no response from the iron (timeout)".to_string())?;
            match head.as_slice() {
                b"OK" => {
                    if !expect_payload {
                        return Ok(Vec::new());
                    }
                    let l = self
                        .read_exact_timeout(2, 1000)?
                        .ok_or("iron said OK but sent no length")?;
                    let n = u16::from_le_bytes([l[0], l[1]]) as usize;
                    if n == 0 {
                        return Ok(Vec::new());
                    }
                    return self
                        .read_exact_timeout(n, 3000)?
                        .ok_or_else(|| format!("iron said OK but {} data bytes did not arrive", n));
                }
                b"PD" => continue,
                b"FL" => {
                    let code = self
                        .read_exact_timeout(2, 500)?
                        .map(|e| format!("0x{:04X}", u16::from_le_bytes([e[0], e[1]])))
                        .unwrap_or_else(|| "?".into());
                    return Err(format!("iron reported error {}", code));
                }
                other => return Err(format!("unexpected reply from iron: {:02X?}", other)),
            }
        }
    }

    fn command(&mut self, cmd: u8, payload: &[u8], checksum: bool, expect_payload: bool, overall_ms: u64) -> FlashResult<Vec<u8>> {
        self.send_command(cmd, payload, checksum)?;
        self.response(expect_payload, overall_ms.min(20_000), overall_ms)
    }

    /// Sync with the BootROM or eflash_loader.
    fn handshake(&mut self, ev: &Sender<FlashEvent>, label: &str) -> FlashResult<()> {
        for attempt in 1..=3 {
            self.clear_input();
            self.write(&[0x55u8; 138])?;
            let reply = self.read_available(300);
            let _ = ev.send(FlashEvent::Log(format!(
                "PineFlash: {} handshake attempt {}: {:02X?}",
                label, attempt, reply
            )));
            if reply.windows(2).any(|w| w == b"OK") {
                sleep(ms(200));
                self.clear_input(); // drop the 'FL' the surplus 'U's produce
                return Ok(());
            }
            if attempt == 2 {
                // blisp's USB reset string, but with the pause blisp leaves out.
                self.write(USB_RESET)?;
                sleep(ms(500));
            }
            sleep(ms(300));
        }
        Err(format!("the iron did not answer the {} handshake", label))
    }

    /// Handshake, then confirm the parser is in step with a boot-info request. The surplus
    /// 'U's can leave the BootROM mid-frame, swallowing the next command; retrying re-aligns it.
    fn sync(&mut self, ev: &Sender<FlashEvent>, label: &str) -> FlashResult<Vec<u8>> {
        let mut last = String::new();
        for attempt in 1..=4 {
            self.handshake(ev, label)?;
            match self.boot_info(1000) {
                Ok(info) => return Ok(info),
                Err(e) => {
                    let stray = self.read_available(100);
                    let _ = ev.send(FlashEvent::Log(format!(
                        "PineFlash: {} sync attempt {} failed ({}), stray {:02X?}; retrying",
                        label, attempt, e, stray
                    )));
                    last = e;
                    sleep(ms(300));
                }
            }
        }
        Err(format!(
            "the iron stopped answering after the {} handshake ({}). Unplug it, then hold [-] while plugging it back in and try again",
            label, last
        ))
    }

    fn boot_info(&mut self, timeout_ms: u64) -> FlashResult<Vec<u8>> {
        self.clear_input();
        self.send_command(0x10, &[], false)?;
        let info = self.response(true, timeout_ms, timeout_ms)?;
        if info.len() < 24 {
            return Err(format!("short boot info ({} bytes)", info.len()));
        }
        Ok(info)
    }

    fn write_memory(&mut self, addr: u32, value: u32, wait: bool) -> FlashResult<()> {
        let mut p = addr.to_le_bytes().to_vec();
        p.extend_from_slice(&value.to_le_bytes());
        self.send_command(0x50, &p, true)?;
        if wait {
            self.response(false, 2000, 2000)?;
        }
        Ok(())
    }

    /// Load the eflash_loader into RAM and start it (blisp_common_prepare_flash for bl70x).
    fn load_loader(mut self, ev: &Sender<FlashEvent>) -> FlashResult<Self> {
        // Uploading to RAM is harmless, so a failed attempt just resyncs and starts over.
        let mut attempt = 1;
        loop {
            match self.upload_loader() {
                Ok(()) => break,
                Err(e) if attempt < 3 => {
                    let _ = ev.send(FlashEvent::Log(format!(
                        "PineFlash: loader upload attempt {} failed ({}); resyncing",
                        attempt, e
                    )));
                    attempt += 1;
                    let info = self.sync(ev, "bootloader")?;
                    if info[0..4] == [0xFF; 4] {
                        break; // a previous attempt got through after all
                    }
                }
                Err(e) => return Err(e),
            }
        }
        let _ = ev.send(FlashEvent::Log("PineFlash: flash-loader started from RAM.".into()));
        sleep(ms(300));
        // Close our handle BEFORE reopening: Windows won't open a COM port twice.
        let name = self.name.clone();
        drop(self);
        let mut dev = Bl70x::reopen(&name)?;
        let info = dev.sync(ev, "loader")?;
        if info[0..4] != [0xFF; 4] {
            return Err("flash-loader did not start".into());
        }
        Ok(dev)
    }

    fn upload_loader(&mut self) -> FlashResult<()> {
        let mut loader = EFLASH_LOADER.to_vec();
        loader[0xE0] = 1; // 32 MHz crystal, as blisp

        sleep(ms(50));
        self.clear_input();
        self.command(0x11, &ram_boot_header(), false, false, 2000)
            .map_err(|e| format!("boot header rejected: {}", e))?;

        let mut seg = Vec::with_capacity(16);
        seg.extend_from_slice(&TCM_ADDRESS.to_le_bytes());
        seg.extend_from_slice(&(loader.len() as u32).to_le_bytes());
        seg.extend_from_slice(&0u32.to_le_bytes());
        let c = crc32(&seg);
        seg.extend_from_slice(&c.to_le_bytes());
        self.command(0x17, &seg, false, true, 2000)
            .map_err(|e| format!("segment header rejected: {}", e))?;

        for chunk in loader.chunks(LOADER_CHUNK) {
            self.command(0x18, chunk, false, false, 3000)
                .map_err(|e| format!("loader upload failed: {}", e))?;
        }
        self.command(0x19, &[], false, false, 3000)
            .map_err(|e| format!("iron rejected the loader image: {}", e))?;

        // run_image, BL70x errata path
        self.write_memory(0x4000_F100, 0x4E42_4845, true)?;
        self.write_memory(0x4000_F104, TCM_ADDRESS, true)?;
        self.write_memory(0x4000_0018, 0x0000_0002, false)?;
        Ok(())
    }

    /// The iron re-enumerates on USB when the loader starts; wait for the port and open it again.
    fn reopen(name: &str) -> FlashResult<Self> {
        sleep(ms(700));
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut last_err = format!("{} not present", name);
        while Instant::now() < deadline {
            let present = serialport::available_ports()
                .map(|ports| ports.iter().any(|p| p.port_name == name))
                .unwrap_or(false);
            if present {
                match Bl70x::open(name) {
                    Ok(dev) => return Ok(dev),
                    Err(e) => last_err = e,
                }
            }
            sleep(ms(300));
        }
        Err(format!("{} did not come back after starting the flash-loader ({})", name, last_err))
    }

    /// Open the port and get to the eflash_loader, whatever state the iron is in.
    pub fn connect(port: &str, ev: &Sender<FlashEvent>) -> FlashResult<Self> {
        let mut dev = Bl70x::open(port)?;
        let info = match dev.boot_info(800) {
            Ok(i) => i,
            Err(_) => dev.sync(ev, "bootloader")?,
        };
        if info[0..4] == [0xFF; 4] {
            let _ = ev.send(FlashEvent::Log("PineFlash: flash-loader already running.".into()));
        } else {
            let id: String = info[16..24].iter().map(|b| format!("{:02X}", b)).collect();
            let _ = ev.send(FlashEvent::Log(format!(
                "PineFlash: BootROM {}.{}.{}.{}, chip ID {}",
                info[0], info[1], info[2], info[3], id
            )));
            dev = dev.load_loader(ev)?;
        }
        Ok(dev)
    }

    // ----- eflash_loader commands -----

    /// Returns (JEDEC ID bytes, flash size in bytes).
    pub fn flash_id(&mut self) -> FlashResult<([u8; 3], u32)> {
        let id = self.command(0x36, &[], true, true, 2000)?;
        if id.len() < 3 {
            return Err("short flash ID".into());
        }
        let cap = id[2];
        if !(0x13..=0x18).contains(&cap) {
            return Err(format!("unexpected flash size code 0x{:02X} (ID {:02X?})", cap, &id[..3]));
        }
        Ok(([id[0], id[1], id[2]], 1u32 << cap))
    }

    pub fn read_flash(&mut self, addr: u32, len: u32, mut progress: impl FnMut(u32)) -> FlashResult<Vec<u8>> {
        let mut out = Vec::with_capacity(len as usize);
        let mut done = 0u32;
        while done < len {
            let n = READ_CHUNK.min(len - done);
            let mut p = (addr + done).to_le_bytes().to_vec();
            p.extend_from_slice(&n.to_le_bytes());
            let data = self
                .command(0x32, &p, true, true, 3000)
                .map_err(|e| format!("flash read at 0x{:06X} failed: {}", addr + done, e))?;
            if data.len() != n as usize {
                return Err(format!("flash read at 0x{:06X} returned {} of {} bytes", addr + done, data.len(), n));
            }
            out.extend_from_slice(&data);
            done += n;
            progress(done);
        }
        Ok(out)
    }

    fn erase(&mut self, start: u32, end: u32) -> FlashResult<()> {
        let mut p = start.to_le_bytes().to_vec();
        p.extend_from_slice(&end.to_le_bytes());
        self.command(0x30, &p, true, false, 120_000)
            .map(|_| ())
            .map_err(|e| format!("erase 0x{:06X}-0x{:06X} failed: {}", start, end, e))
    }

    fn write_flash(&mut self, addr: u32, data: &[u8], mut progress: impl FnMut(usize)) -> FlashResult<()> {
        let mut sent = 0usize;
        for chunk in data.chunks(WRITE_CHUNK) {
            let a = addr + sent as u32;
            let mut p = a.to_le_bytes().to_vec();
            p.extend_from_slice(chunk);
            self.command(0x31, &p, true, false, 5000)
                .map_err(|e| format!("write at 0x{:06X} failed: {}", a, e))?;
            sent += chunk.len();
            progress(sent);
        }
        Ok(())
    }

    fn program_check(&mut self) -> FlashResult<()> {
        self.command(0x3A, &[], true, false, 5000)
            .map(|_| ())
            .map_err(|e| format!("program check failed: {}", e))
    }

    fn reset(&mut self) {
        // The iron may reboot before it answers.
        let _ = self.send_command(0x21, &[], true);
        let _ = self.response(false, 1000, 1000);
    }
}

// ---------------------------------------------------------------------------
// High-level operations
// ---------------------------------------------------------------------------

fn compare(a: &[u8], b: &[u8], base: u32, what: &str) -> FlashResult<()> {
    match a.iter().zip(b).position(|(x, y)| x != y) {
        None if a.len() == b.len() => Ok(()),
        None => Err(format!("{}: length mismatch", what)),
        Some(i) => Err(format!(
            "{}: mismatch at 0x{:06X} (flash {:02X}, expected {:02X})",
            what, base as usize + i, a[i], b[i]
        )),
    }
}

fn unix_time() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Read the whole flash and save it. Returns (path, contents).
fn backup(dev: &mut Bl70x, size: u32, dir: &Path, ev: &Sender<FlashEvent>) -> FlashResult<(PathBuf, Vec<u8>)> {
    let _ = ev.send(FlashEvent::Stage("Backing up current firmware".into()));
    let data = dev.read_flash(0, size, |done| {
        let _ = ev.send(FlashEvent::Progress(done as f32 / size as f32));
    })?;
    // Reads must be stable before we trust the backup.
    let again = dev.read_flash(FIRMWARE_ADDRESS, READ_CHUNK, |_| {})?;
    compare(&again, &data[FIRMWARE_ADDRESS as usize..(FIRMWARE_ADDRESS + READ_CHUNK) as usize], FIRMWARE_ADDRESS, "backup re-read")?;

    fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {}", dir.display(), e))?;
    let path = dir.join(format!("pinecil-v2-backup-{}.bin", unix_time()));
    fs::write(&path, &data).map_err(|e| format!("could not save backup {}: {}", path.display(), e))?;
    let on_disk = fs::read(&path).map_err(|e| format!("could not re-read backup: {}", e))?;
    compare(&on_disk, &data, 0, "saved backup")?;
    let _ = ev.send(FlashEvent::Log(format!("PineFlash: backup of {} bytes saved to {}", size, path.display())));
    Ok((path, data))
}

/// Flash firmware to 0x2000, with backup, protected-region checks and full read-back verify.
pub fn flash_firmware(job: FlashJob, ev: &Sender<FlashEvent>) -> FlashResult<String> {
    let fw = &job.firmware;
    let _ = ev.send(FlashEvent::Stage("Connecting to the iron".into()));
    let mut dev = Bl70x::connect(&job.port, ev)?;

    let (id, size) = dev.flash_id()?;
    let settings = size - SECTOR; // IronOS keeps its settings in the last sector
    let end = FIRMWARE_ADDRESS + fw.len() as u32;
    let _ = ev.send(FlashEvent::Log(format!(
        "PineFlash: flash {:02X?}, {} KiB; firmware {} ({} bytes) -> 0x{:06X}-0x{:06X}",
        id, size / 1024, job.firmware_name, fw.len(), FIRMWARE_ADDRESS, end
    )));
    if fw.is_empty() || end >= settings {
        return Err(format!("firmware does not fit below the settings sector (ends at 0x{:06X})", end));
    }

    let (backup_path, before) = backup(&mut dev, size, &job.backup_dir, ev)?;

    // Boot header: keep it if it's already blisp's, otherwise write it like blisp would.
    let header_ok = header_matches(&before[..176]);
    if header_ok {
        let _ = ev.send(FlashEvent::Log("PineFlash: boot header is valid; leaving it untouched.".into()));
    } else {
        let _ = ev.send(FlashEvent::Stage("Writing boot header".into()));
        let header = flash_boot_header();
        dev.erase(0, header.len() as u32)?;
        dev.write_flash(0, &header, |_| {})?;
        let check = dev.read_flash(0, 176, |_| {})?;
        compare(&check, &header, 0, "boot header")?;
        let _ = ev.send(FlashEvent::Log("PineFlash: boot header was missing; written and verified.".into()));
    }

    let _ = ev.send(FlashEvent::Stage("Erasing".into()));
    let _ = ev.send(FlashEvent::Progress(0.0));
    dev.erase(FIRMWARE_ADDRESS, end)?;

    let _ = ev.send(FlashEvent::Stage("Writing firmware".into()));
    dev.write_flash(FIRMWARE_ADDRESS, fw, |sent| {
        let _ = ev.send(FlashEvent::Progress(sent as f32 / fw.len() as f32));
    })?;
    dev.program_check()?;

    let _ = ev.send(FlashEvent::Stage("Verifying".into()));
    let written = dev.read_flash(FIRMWARE_ADDRESS, fw.len() as u32, |done| {
        let _ = ev.send(FlashEvent::Progress(done as f32 / fw.len() as f32));
    })?;
    compare(&written, fw, FIRMWARE_ADDRESS, "firmware verify")?;

    // Protected regions must be exactly as before (or as just written, for the header).
    if header_ok {
        let h = dev.read_flash(0, FIRMWARE_ADDRESS, |_| {})?;
        compare(&h, &before[..FIRMWARE_ADDRESS as usize], 0, "boot header sector")?;
    }
    let s = dev.read_flash(settings, SECTOR, |_| {})?;
    compare(&s, &before[settings as usize..], settings, "settings sector")?;

    dev.reset();
    Ok(format!(
        "Flashed and verified {} bytes. Backup: {}",
        fw.len(),
        backup_path.display()
    ))
}

/// Put a previous backup's firmware back (header and settings are handled as in flash_firmware).
pub fn restore_backup(port: &str, backup_file: &Path, backup_dir: PathBuf, ev: &Sender<FlashEvent>) -> FlashResult<String> {
    let data = fs::read(backup_file).map_err(|e| format!("could not read {}: {}", backup_file.display(), e))?;
    if !data.len().is_power_of_two() || data.len() < 0x80000 {
        return Err(format!("{} is not a full flash backup ({} bytes)", backup_file.display(), data.len()));
    }
    let settings = data.len() - SECTOR as usize;
    let mut used = settings;
    while used > FIRMWARE_ADDRESS as usize && data[used - 1] == 0xFF {
        used -= 1;
    }
    if used <= FIRMWARE_ADDRESS as usize {
        return Err("backup contains no firmware".into());
    }
    flash_firmware(
        FlashJob {
            port: port.to_string(),
            firmware: data[FIRMWARE_ADDRESS as usize..used].to_vec(),
            firmware_name: backup_file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            backup_dir,
        },
        ev,
    )
}

/// Run on a background thread; always ends with a Finished event.
pub fn spawn<F>(ev: Sender<FlashEvent>, f: F)
where
    F: FnOnce(&Sender<FlashEvent>) -> FlashResult<String> + Send + 'static,
{
    std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&ev)))
            .unwrap_or_else(|_| Err("internal error while flashing".into()));
        let _ = ev.send(FlashEvent::Finished(result));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_known_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn header_crcs_match_blisp() {
        assert_eq!(crc32(&RAM_FLASH_CFG), RAM_FLASH_CFG_CRC);
        assert_eq!(crc32(&RAM_CLK_CFG), RAM_CLK_CFG_CRC);
        assert_eq!(crc32(&FLASH_FLASH_CFG), FLASH_FLASH_CFG_CRC);
        assert_eq!(crc32(&FLASH_CLK_CFG), FLASH_CLK_CFG_CRC);
    }

    #[test]
    fn headers_are_176_bytes_with_magic() {
        let r = ram_boot_header();
        assert_eq!(&r[0..4], b"BFNP");
        assert_eq!(&r[8..12], b"FCFG");
        assert_eq!(&r[0x64..0x68], b"PCFG");
        let f = flash_boot_header();
        assert_eq!(&f[0..4], b"BFNP");
        assert_eq!(u32::from_le_bytes(f[128..132].try_into().unwrap()), FIRMWARE_ADDRESS);
    }

    #[test]
    fn header_match_ignores_bytes_blisp_leaves_unset() {
        let mut h = flash_boot_header();
        assert!(header_matches(&h));
        h[0x64..0x68].copy_from_slice(b"PCFG");
        h[0x6A] = 0x55;
        assert!(header_matches(&h));
        h[0x20] ^= 1;
        assert!(!header_matches(&h));
        assert!(!header_matches(&[0xFF; 176]));
    }

    /// First 64 bytes read from a real Pinecil V2 flashed by blisp.
    #[test]
    fn header_matches_real_iron() {
        let real: [u8; 64] = [
            0x42, 0x46, 0x4E, 0x50, 0x01, 0x00, 0x00, 0x00, 0x46, 0x43, 0x46, 0x47, 0x11, 0x00, 0x01, 0x01,
            0x66, 0x99, 0xFF, 0x03, 0x9F, 0x00, 0x9F, 0x00, 0x04, 0xC2, 0x00, 0x01, 0xC7, 0x20, 0x52, 0xD8,
            0x06, 0x02, 0x32, 0x00, 0x0B, 0x01, 0x0B, 0x01, 0x3B, 0x01, 0xBB, 0x00, 0x6B, 0x01, 0xEB, 0x02,
            0xEB, 0x02, 0x02, 0x50, 0x00, 0x01, 0x00, 0x01, 0x01, 0x00, 0x02, 0x01, 0x02, 0x01, 0xAB, 0x01,
        ];
        assert_eq!(&flash_boot_header()[..64], &real[..]);
    }

    /// Hardware test, READ-ONLY: connect, read flash ID, back up the whole flash.
    /// Run with the iron in flashing mode:
    ///   PINECIL_PORT=COM3 cargo test live_read_only -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_read_only() {
        let port = std::env::var("PINECIL_PORT").unwrap_or_else(|_| "COM3".into());
        let (tx, rx) = std::sync::mpsc::channel();
        let printer = std::thread::spawn(move || {
            for e in rx {
                if let FlashEvent::Log(s) | FlashEvent::Stage(s) = e {
                    println!("{}", s);
                }
            }
        });
        let result = (|| -> FlashResult<()> {
            let mut dev = Bl70x::connect(&port, &tx)?;
            let (id, size) = dev.flash_id()?;
            println!("flash ID {:02X?}, {} KiB", id, size / 1024);
            let dir = std::env::temp_dir().join("pineflash-live-test");
            let (path, data) = backup(&mut dev, size, &dir, &tx)?;
            println!("backup ok: {} ({} bytes)", path.display(), data.len());
            println!("boot header valid: {}", header_matches(&data[..176]));
            Ok(())
        })();
        drop(tx);
        printer.join().unwrap();
        result.unwrap();
    }

    #[test]
    fn loader_is_blisps() {
        assert_eq!(EFLASH_LOADER.len(), 59200);
        assert_eq!(&EFLASH_LOADER[..4], &[0x97, 0x41, 0x01, 0x20]);
    }
}
