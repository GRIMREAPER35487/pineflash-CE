use crate::Flasher;
use serialport::SerialPortType::UsbPort;

impl Flasher {
    pub fn check_connections(&mut self) -> Option<String> {
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
        detected
    }
}
