use std::fs::File;
use std::io::{Cursor, Read};
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::submodules::bl70x::{self, FlashEvent, FlashJob};
use crate::Flasher;

/// Where firmware backups are kept: ~/PineFlash/backups (or the temp dir if there is no home).
pub fn backup_dir() -> PathBuf {
    simple_home_dir::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("PineFlash")
        .join("backups")
}

/// Directory holding bundled tools: next to the executable, not the current directory.
#[allow(dead_code)]
pub fn tools_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
        .join("tools")
}

impl Flasher {
    /// Extract the downloaded release (or take the custom file) and return the firmware path.
    fn prepare_firmware_path(&mut self) -> Result<String, String> {
        if self.config.version == *"Custom" {
            return self
                .config
                .picked_path
                .clone()
                .ok_or_else(|| "Please select a file or firmware version".to_string());
        }
        let name = format!("{}-{}", self.config.version, self.config.int_name);
        let zip: PathBuf = std::env::temp_dir().join(format!("{}.zip", name));
        let target: PathBuf = std::env::temp_dir().join(&name);

        let mut data = Vec::new();
        File::open(&zip)
            .and_then(|mut f| f.read_to_end(&mut data))
            .map_err(|e| format!("Could not open the downloaded firmware {}: {}", zip.display(), e))?;
        zip_extract::extract(Cursor::new(data), &target, true)
            .map_err(|e| format!("The downloaded firmware is not a valid zip ({}). Try again later.", e))?;
        self.config.logs.push_str("PineFlash: File extracted successfully\n");

        let ext = if self.config.int_name == "Pinecil" { "dfu" } else { "bin" };
        let file = target.join(format!("{}_{}.{}", self.config.int_name, self.config.lang, ext));
        if !file.exists() {
            return Err(format!("{} is not in this release", file.display()));
        }
        Ok(file.to_string_lossy().into_owned())
    }

    pub fn flash(&mut self) {
        let firmware_path = match self.prepare_firmware_path() {
            Ok(p) => p,
            Err(e) => {
                self.flash_failed(&e);
                self.reset_after_flash();
                return;
            }
        };

        self.config.logs.push_str(
            format!(
                "PineFlash: Attempting to flash {} with the firmware {}\n",
                self.config.int_name, firmware_path
            )
            .as_str(),
        );

        #[cfg(target_os = "windows")]
        if self.config.int_name == "Pinecilv2" {
            self.start_native_flash(firmware_path);
            return;
        }

        if self.config.int_name == "Pinecil" {
            self.flash_v1(firmware_path);
        } else if self.config.int_name == "Pinecilv2" {
            self.flash_v2_blisp(firmware_path);
        }
        self.reset_after_flash();
    }

    fn flash_failed(&mut self, msg: &str) {
        self.toasts.dismiss_all_toasts();
        self.toasts
            .error(format!("Flashing failed\n{}", msg))
            .set_duration(Some(Duration::from_secs(8)))
            .set_closable(true);
        self.config.logs.push_str(format!("PineFlash: {}\n", msg).as_str());
    }

    /// Very ugly way of resetting the program
    pub fn reset_after_flash(&mut self) {
        self.config.version = "Select".to_string();
        self.config.fancy_names = vec![];
        self.config.code_names = vec![];
        self.config.download = false;
        self.config.run_once_vers = true;
        self.config.download_firm_notify = true;
        self.config.picked_path = None;
        self.config.ready_to_flash = false;
        self.config.flash_notified_count = 0;
        self.config.flash = false;
    }

    // ----------------------------------------------------------------------
    // Pinecil V2, built-in flasher (Windows)
    // ----------------------------------------------------------------------
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    fn start_native_flash(&mut self, firmware_path: String) {
        if !firmware_path.to_lowercase().ends_with(".bin") {
            self.flash_failed("The built-in Pinecil V2 flasher only accepts .bin firmware files.");
            self.reset_after_flash();
            return;
        }
        let Some(port) = self.config.v2_serial_path.clone() else {
            self.flash_failed("No Pinecil V2 in flashing mode was found.");
            self.reset_after_flash();
            return;
        };
        let firmware = match std::fs::read(&firmware_path) {
            Ok(f) => f,
            Err(e) => {
                self.flash_failed(&format!("Could not read {}: {}", firmware_path, e));
                self.reset_after_flash();
                return;
            }
        };
        let job = FlashJob {
            port,
            firmware,
            firmware_name: std::path::Path::new(&firmware_path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            backup_dir: backup_dir(),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        bl70x::spawn(tx, move |ev| bl70x::flash_firmware(job, ev));
        self.native_job_started(rx, "Flashing");
        self.config.flash = false;
    }

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub fn start_restore(&mut self, backup_file: PathBuf) {
        let Some(port) = self.config.v2_serial_path.clone() else {
            self.flash_failed("No Pinecil V2 in flashing mode was found.");
            return;
        };
        self.config.logs.push_str(
            format!("PineFlash: Restoring backup {}\n", backup_file.display()).as_str(),
        );
        let (tx, rx) = std::sync::mpsc::channel();
        bl70x::spawn(tx, move |ev| bl70x::restore_backup(&port, &backup_file, backup_dir(), ev));
        self.native_job_started(rx, "Restoring backup");
    }

    fn native_job_started(&mut self, rx: std::sync::mpsc::Receiver<FlashEvent>, what: &str) {
        self.config.flash_events = Some(rx);
        self.config.flash_stage = format!("{}...", what);
        self.config.flash_progress = 0.0;
        self.toasts.dismiss_all_toasts();
        self.toasts
            .info(format!("{}\nKeep the iron plugged in.", what))
            .set_duration(None)
            .set_closable(false);
    }

    /// Called every frame: pull events from the flashing thread.
    pub fn poll_native_flash(&mut self) {
        let mut finished = None;
        if let Some(rx) = &self.config.flash_events {
            while let Ok(event) = rx.try_recv() {
                match event {
                    FlashEvent::Log(s) => {
                        self.config.logs.push_str(&s);
                        self.config.logs.push('\n');
                    }
                    FlashEvent::Stage(s) => {
                        self.config.logs.push_str(format!("PineFlash: {}...\n", s).as_str());
                        self.config.flash_stage = s;
                        self.config.flash_progress = 0.0;
                    }
                    FlashEvent::Progress(p) => self.config.flash_progress = p.clamp(0.0, 1.0),
                    FlashEvent::Finished(r) => finished = Some(r),
                }
            }
        }
        let Some(result) = finished else { return };
        self.config.flash_events = None;
        self.toasts.dismiss_all_toasts();
        match result {
            Ok(msg) => {
                self.config.logs.push_str(format!("PineFlash: {}\n", msg).as_str());
                self.toasts
                    .success("Flashed and verified\nYour iron restarts into the new firmware. If it doesn't, unplug it and plug it back in.")
                    .set_duration(Some(Duration::from_secs(8)))
                    .set_closable(true);
            }
            Err(e) => {
                self.flash_failed(&e);
                self.config.logs.push_str(
                    format!(
                        "PineFlash: Your iron's bootloader cannot be erased; hold [-] while plugging in and try again. Backups are in {}\n",
                        backup_dir().display()
                    )
                    .as_str(),
                );
            }
        }
        self.reset_after_flash();
    }

    // ----------------------------------------------------------------------
    // Pinecil V1 (dfu-util)
    // ----------------------------------------------------------------------
    fn flash_v1(&mut self, firmware_path: String) {
        #[cfg(feature = "appimage")]
        let dfupath = {
            let tool_path: PathBuf = std::env::temp_dir().join("flash_tools");
            let _ = std::fs::create_dir(&tool_path);
            let dfupath = tool_path.join("dfu-util");
            let src = std::env::current_dir().unwrap_or_default().join("appimage").join("dfu-util");
            if let Err(e) = std::fs::copy(&src, &dfupath) {
                self.flash_failed(&format!("Could not prepare dfu-util: {}", e));
                return;
            }
            dfupath
        };
        #[cfg(all(not(feature = "appimage"), target_os = "linux"))]
        let dfupath = "dfu-util";

        #[cfg(target_os = "linux")]
        let command = Command::new("pkexec").arg(dfupath).arg("-D").arg(&firmware_path).arg("-a").arg("0").output();
        #[cfg(target_os = "macos")]
        let command = Command::new("dfu-util").arg("-D").arg(&firmware_path).arg("-a").arg("0").output();
        #[cfg(target_os = "windows")]
        let command = Command::new(tools_dir().join("dfu-util.exe"))
            .creation_flags(0x00000008)
            .arg("-D")
            .arg(&firmware_path)
            .arg("-a")
            .arg("0")
            .output();

        let command = match command {
            Ok(c) => c,
            Err(e) => {
                self.flash_failed(&format!("Could not run dfu-util: {}", e));
                return;
            }
        };
        let output = String::from_utf8_lossy(&command.stdout).into_owned();
        let output_err = String::from_utf8_lossy(&command.stderr).into_owned();
        self.toasts.dismiss_all_toasts();
        if command.status.success() {
            self.toasts
                .success("Flashing complete")
                .set_duration(Some(Duration::from_secs(5)))
                .set_closable(false);
        } else {
            self.toasts
                .error("Flashing failed\nIs your Pinecil plugged in?")
                .set_duration(Some(Duration::from_secs(5)))
                .set_closable(false);
        }
        self.config
            .logs
            .push_str(format!("Dfu-Util: {}{}\n", output, output_err).as_str());
    }

    // ----------------------------------------------------------------------
    // Pinecil V2 via blisp (Linux / macOS)
    // ----------------------------------------------------------------------
    #[cfg_attr(target_os = "windows", allow(dead_code, unused_variables))]
    fn flash_v2_blisp(&mut self, firmware_path: String) {
        #[cfg(not(target_os = "windows"))]
        {
            #[cfg(feature = "appimage")]
            let blisppath = {
                let tool_path: PathBuf = std::env::temp_dir().join("flash_tools");
                let _ = std::fs::create_dir(&tool_path);
                let blisppath = tool_path.join("blisp");
                let src = std::env::current_dir().unwrap_or_default().join("linux").join("blisp");
                if let Err(e) = std::fs::copy(&src, &blisppath) {
                    self.flash_failed(&format!("Could not prepare blisp: {}", e));
                    return;
                }
                blisppath
            };
            #[cfg(all(not(feature = "appimage"), target_os = "linux"))]
            let blisppath = "blisp";

            #[cfg(target_os = "linux")]
            let command = {
                let Some(port) = self.config.v2_serial_path.clone() else {
                    self.flash_failed("No Pinecil V2 in flashing mode was found.");
                    return;
                };
                Command::new("pkexec")
                    .env("PATH", "/usr/local/sbin:/usr/local/bin:/usr/bin:/bin")
                    .arg(blisppath)
                    .args(["write", "-c", "bl70x", "-p"])
                    .arg(port)
                    .arg("--reset")
                    .arg(&firmware_path)
                    .output()
            };
            #[cfg(target_os = "macos")]
            let command = Command::new("blisp")
                .args(["write", "-c", "bl70x", "--reset"])
                .arg(&firmware_path)
                .output();

            let command = match command {
                Ok(c) => c,
                Err(e) => {
                    self.flash_failed(&format!("Could not run blisp: {}", e));
                    return;
                }
            };
            let output = String::from_utf8_lossy(&command.stdout).into_owned();
            let output_err = String::from_utf8_lossy(&command.stderr).into_owned();
            self.toasts.dismiss_all_toasts();
            // blisp can exit 0 on failure, so check its output too.
            let failed = !command.status.success()
                || ["Device not found", "Failed", "Error", "Not authorized"]
                    .iter()
                    .any(|s| output_err.contains(s) || output.contains(s));
            if !failed && output.contains("Flash complete") {
                self.toasts
                    .success("Flashing complete")
                    .set_duration(Some(Duration::from_secs(4)))
                    .set_closable(false);
            } else if output_err.contains("Not authorized") {
                self.toasts
                    .error("Could not run as admin\nWas the password wrong?")
                    .set_duration(Some(Duration::from_secs(4)))
                    .set_closable(false);
            } else {
                self.toasts
                    .error("Flashing failed\nSee the log for details.")
                    .set_duration(Some(Duration::from_secs(4)))
                    .set_closable(false);
            }
            self.config
                .logs
                .push_str(format!("Blisp: {}{}\n", output, output_err).as_str());
        }
    }
}
