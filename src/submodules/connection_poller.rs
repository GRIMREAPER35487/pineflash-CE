use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use crate::Flasher;
use serialport::SerialPortType::UsbPort;

/// Result of one USB scan: (Pinecil V1 in DFU mode present, Pinecil V2 bootloader port).
pub type Scan = (bool, Option<String>);

const POLL_EVERY: Duration = Duration::from_millis(1500);

/// Look for irons in flashing mode. This asks Windows/the OS for its USB device and serial
/// port lists, which can take seconds when a device is misbehaving, so it runs off the UI thread.
pub fn scan() -> Scan {
    // Pinecil V1 in DFU mode (GD32VF103 bootloader)
    let v1 = match rusb::devices() {
        Ok(devices) => devices.iter().any(|device| {
            device
                .device_descriptor()
                .map(|d| d.vendor_id() == 0x28E9 && d.product_id() == 0x0189)
                .unwrap_or(false)
        }),
        Err(_) => false,
    };

    // Pinecil V2 in flashing mode (BL70x USB bootloader: VID/PID FFFF:FFFF)
    let mut v2_port = None;
    if let Ok(ports) = serialport::available_ports() {
        for device in ports {
            if let UsbPort(info) = device.port_type {
                let is_bootloader = info.vid == 0xFFFF && info.pid == 0xFFFF;
                let serial_match = info
                    .serial_number
                    .as_deref()
                    .map(|s| s.contains("000000020000"))
                    .unwrap_or(false);
                if is_bootloader || serial_match {
                    v2_port = Some(device.port_name.clone());
                }
            }
        }
    }
    (v1, v2_port)
}

impl Flasher {
    /// Called every frame: picks up a finished scan and starts the next one when due.
    /// The window never waits on the USB stack; a slow scan only delays the status update.
    pub fn poll_connections(&mut self) {
        if let Some(rx) = &self.config.poll_rx {
            match rx.try_recv() {
                Ok((v1, v2_port)) => {
                    self.config.poll_rx = None;
                    // Ignore results that raced with a flash starting: the iron re-enumerates mid-flash.
                    if self.config.flash_events.is_none() {
                        self.apply_scan(v1, v2_port);
                    }
                }
                Err(TryRecvError::Empty) => return, // still scanning
                Err(TryRecvError::Disconnected) => self.config.poll_rx = None,
            }
        }
        if self.config.flash_events.is_some() || self.config.last_poll.elapsed() < POLL_EVERY {
            return;
        }
        self.config.last_poll = Instant::now();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(scan());
        });
        self.config.poll_rx = Some(rx);
        self.config.backup_count = super::main_panel::count_backups();
    }

    fn apply_scan(&mut self, v1: bool, v2_port: Option<String>) {
        let v2 = v2_port.is_some();
        // Never keep a stale port from an earlier connection.
        self.config.v2_serial_path = v2_port;

        let detected = match (v1, v2) {
            (true, true) => Some("Both".to_string()),
            (true, false) => Some("Pinecil".to_string()),
            (false, true) => Some("Pinecilv2".to_string()),
            (false, false) => None,
        };

        if detected != self.config.iron_connected {
            match (&detected, &self.config.iron_connected) {
                (Some(d), _) => {
                    self.config.logs.push_str(format!("PineFlash: {} detected\n", d).as_str());
                    if let Some(port) = &self.config.v2_serial_path {
                        self.config
                            .logs
                            .push_str(format!("PineFlash: Serial port is {} \n", port).as_str());
                    }
                }
                (None, Some(old)) => {
                    self.config
                        .logs
                        .push_str(format!("PineFlash: {} disconnected\n", old).as_str());
                }
                (None, None) => {}
            }
        }
        self.config.iron_connected = detected;
    }
}
