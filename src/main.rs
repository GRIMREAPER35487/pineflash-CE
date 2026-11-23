#![windows_subsystem = "windows"]
use std::{
    collections::HashMap,
    env,
    fs::{self, File},
    io::{Cursor, Read},
    path::PathBuf,
    time::Duration,
};

use curl::easy::Easy;
use eframe::egui;
use eframe::{
    epaint::{Color32, Rounding, Stroke},
    CreationContext,
};
mod submodules;
use egui::Context;
use submodules::notify::Toasts;
use serde::{Deserialize, Serialize};
use serde_json::Value;


#[derive(Serialize, Deserialize)]
struct Language {
    language_code: String,
    language_name: String,
}

#[derive(Serialize, Deserialize)]
struct YourValue {
    contents: HashMap<String, Language>,
}

struct FlasherConfig {
    iron: String,
    int_name: String,
    version: String,
    fancy_names: Vec<String>,
    code_names: Vec<String>,
    lang: String,
    versions_checked: bool,
    vers: Vec<String>,
    /// Which entries of `vers` are pre-releases.
    prereleases: Vec<String>,
    download_metadata: bool,
    blisp_version: String,
    run_once_vers: bool,
    download: bool,
    picked_path: Option<String>,
    download_versions: bool,
    download_firm_notify: bool,
    ready_to_flash: bool,
    logs: String,
    json: String,
    iron_connected: Option<String>,
    last_poll: std::time::Instant,
    /// A USB scan running in the background, if any.
    poll_rx: Option<std::sync::mpsc::Receiver<submodules::connection_poller::Scan>>,
    flash: bool,
    flash_notified_count: i32,
    v2_serial_path: Option<String>,
    // connection_guide_image: Vec<ColorImage>,
    current_step: usize,
    /// The IronOS release list request running in the background, if any.
    versions_rx: Option<std::sync::mpsc::Receiver<Result<(String, submodules::ghcache::Freshness), String>>>,
    metadata_path: PathBuf,
    /// A native Open dialog running on a background thread, and what the file is for.
    picker: Option<(submodules::main_panel::Pick, std::sync::mpsc::Receiver<Option<PathBuf>>)>,
    // Built-in Pinecil V2 flasher (runs on a background thread)
    flash_events: Option<std::sync::mpsc::Receiver<submodules::bl70x::FlashEvent>>,
    flash_stage: String,
    flash_progress: f32,
    last_iron: String,
    last_version: String,
    /// None = automatic (shown while no iron is connected)
    show_guide: Option<bool>,
    backup_count: usize,
    /// Boot logo mode instead of firmware mode.
    logo_mode: bool,
    logo: submodules::logos::LogoState,
    /// Custom firmware sources: fetched release lists and the manager window.
    sources: submodules::sources::SourcesState,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)] // settings files from older versions are missing newer fields
struct FlashSavedConfig {
    pub dark_mode: bool,
    /// List IronOS pre-releases (release candidates) newer than the latest stable release.
    pub show_prereleases: bool,
    /// Extra GitHub repositories ("owner/repo") whose releases are offered as firmware.
    pub sources: Vec<String>,
}

impl Default for FlashSavedConfig {
    fn default() -> Self {
        Self { dark_mode: true, show_prereleases: false, sources: Vec::new() }
    }
}

/// From GitHub's release list (newest first): the three newest stable releases, plus, if asked,
/// any pre-releases newer than the newest stable one. Returns (versions, the pre-release ones).
fn parse_versions(json: &str, include_prereleases: bool) -> (Vec<String>, Vec<String>) {
    let releases = serde_json::from_str::<Value>(json)
        .ok()
        .and_then(|j| j.as_array().cloned())
        .unwrap_or_default();
    let mut versions = Vec::new();
    let mut pre = Vec::new();
    let mut stable = 0;
    for r in releases.iter().filter(|r| !r["draft"].as_bool().unwrap_or(false)) {
        let Some(tag) = r["tag_name"].as_str() else { continue };
        if r["prerelease"].as_bool().unwrap_or(false) {
            if include_prereleases && stable == 0 {
                versions.push(tag.to_string());
                pre.push(tag.to_string());
            }
        } else {
            versions.push(tag.to_string());
            stable += 1;
            if stable == 3 {
                break;
            }
        }
    }
    (versions, pre)
}

impl Flasher {
    pub fn save_settings(&mut self) {
        if confy::store("PineFlash", None, self.saved_config.clone()).is_err() {
            self.config.logs.push_str("PineFlash: Failed to save the app state.\n");
        }
    }

    /// Re-read the cached release list, e.g. after the pre-release switch changes.
    pub fn refresh_versions(&mut self) {
        let json = std::fs::read_to_string(&self.config.metadata_path).unwrap_or_default();
        let (versions, pre) = parse_versions(&json, self.saved_config.show_prereleases);
        if versions.is_empty() {
            return;
        }
        if self.config.version != "Select"
            && self.config.version != "Custom"
            && !submodules::sources::is_custom(&self.config.version)
            && !versions.contains(&self.config.version)
        {
            self.config.version = "Select".to_string();
        }
        self.config.vers = versions;
        self.config.prereleases = pre;
    }
}

struct Flasher {
    config: FlasherConfig,
    saved_config: FlashSavedConfig,
    toasts: Toasts,
}

impl Default for FlasherConfig {
    fn default() -> Self {
        Self {
            iron: "Pinecil V2".to_string(),
            int_name: "Pinecilv2".to_string(),
            version: "Select".to_string(),
            fancy_names: vec![],
            code_names: vec![],
            lang: "EN".to_string(),
            versions_checked: false,
            vers: vec![],
            prereleases: vec![],
            download_metadata: false,
            run_once_vers: true,
            versions_rx: None,
            download: false,
            blisp_version: "".to_string(),
            download_versions: true,
            download_firm_notify: true,
            picked_path: None,
            ready_to_flash: false,
            #[cfg(feature = "appimage")]
            logs: format!("PineFlash CE v{} Linux Appimage\n", env!("CARGO_PKG_VERSION")),
            #[cfg(not(feature = "appimage"))]
            #[cfg(target_os = "linux")]
            logs: format!("PineFlash CE v{} Linux Native\n", env!("CARGO_PKG_VERSION")),
            #[cfg(target_os = "windows")]
            logs: format!("PineFlash CE v{} Windows\n", env!("CARGO_PKG_VERSION")),
            #[cfg(target_os = "macos")]
            logs: format!("PineFlash CE v{} MacOs\n", env!("CARGO_PKG_VERSION")),
            json: "".to_string(),
            iron_connected: None,
            // Scan right away at startup instead of after the first interval.
            last_poll: std::time::Instant::now() - Duration::from_secs(10),
            poll_rx: None,
            flash: false,
            flash_notified_count: 0,
            v2_serial_path: None,
            // connection_guide_image: [
                //
                // RetainedImage::from_svg_bytes("Step1", include_bytes!("../assets/Step1.svg"))
                //     .unwrap(),
                // RetainedImage::from_svg_bytes("Step2", include_bytes!("../assets/Step2.svg"))
                //     .unwrap(),
                // RetainedImage::from_svg_bytes("Step3", include_bytes!("../assets/Step3.svg"))
                //     .unwrap(),
            // ],
            current_step: 0,
            metadata_path: [std::env::temp_dir(), "metadata.json".into()]
                .iter()
                .collect(),
            flash_events: None,
            flash_stage: String::new(),
            flash_progress: 0.0,
            picker: None,
            last_iron: "Pinecil V2".to_string(),
            last_version: "Select".to_string(),
            show_guide: None,
            backup_count: submodules::main_panel::count_backups(),
            logo_mode: false,
            logo: Default::default(),
            sources: Default::default(),
        }
    }
}

impl Flasher {
    fn new(cc: &CreationContext) -> Flasher {
        let config: FlasherConfig = FlasherConfig::default();
        // Flasher::configure_fonts(&cc.egui_ctx);
        let saved_config: FlashSavedConfig = confy::load("PineFlash", None).unwrap_or_default();
        let toasts = Toasts::default();

        apply_theme(&cc.egui_ctx, saved_config.dark_mode);
        // Follow the OS display scaling, just a little larger than egui's default.
        cc.egui_ctx.set_zoom_factor(1.15);
        Flasher::configure_fonts(cc.egui_ctx.clone());
        Flasher {
            config,
            toasts,
            saved_config,
        }
    }
}

impl eframe::App for Flasher {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // Wake up regularly to poll USB and background work, without spinning the CPU.
        ctx.request_repaint_after(Duration::from_millis(100));

        self.poll_native_flash();

        // USB scan on a background thread (not while flashing: the iron re-enumerates mid-flash).
        self.poll_connections();
        // Fetch release lists for custom firmware sources in the background.
        self.poll_sources();
        self.poll_picker();

        if self.config.download_versions {
            self.config.download_versions = false;
            self.toasts
                .info("Checking for firmware releases")
                .set_duration(None)
                .set_closable(false);
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(submodules::ghcache::get_json(
                    "https://api.github.com/repos/Ralim/IronOS/releases",
                    "ironos-releases",
                ));
            });
            self.config.versions_rx = Some(rx);
        }
        let versions_reply = match &self.config.versions_rx {
            Some(rx) => match rx.try_recv() {
                Ok(r) => Some(r),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err("the request stopped".into())),
            },
            None => None,
        };
        if let Some(reply) = versions_reply {
            use submodules::ghcache::Freshness;
            self.config.versions_rx = None;
            self.config.versions_checked = true;
            self.toasts.dismiss_all_toasts();
            let parsed = reply.and_then(|(json, fresh)| {
                let (versions, pre) = parse_versions(&json, self.saved_config.show_prereleases);
                if versions.is_empty() {
                    Err("GitHub's release list had no releases in it".to_string())
                } else {
                    Ok((json, versions, pre, fresh))
                }
            });
            match parsed {
                Ok((json, versions, pre, fresh)) => {
                    // refresh_versions() re-reads this when the pre-release switch changes.
                    let _ = std::fs::write(&self.config.metadata_path, &json);
                    self.config.vers = versions;
                    self.config.prereleases = pre;
                    self.config.download_metadata = true;
                    match fresh {
                        Freshness::Live | Freshness::Unchanged => {
                            self.config.logs.push_str("PineFlash: Versions successfully fetched.\n");
                            self.toasts
                                .success("Firmware list updated")
                                .set_duration(Some(Duration::from_secs(5)));
                        }
                        Freshness::Saved(why) => {
                            self.config
                                .logs
                                .push_str(format!("PineFlash: Using the saved firmware list ({}).\n", why).as_str());
                            self.toasts
                                .warning(format!("Using the saved firmware list\n{}.", why))
                                .set_duration(Some(Duration::from_secs(7)))
                                .set_closable(true);
                        }
                    }
                }
                Err(why) => {
                    self.config
                        .logs
                        .push_str(format!("PineFlash: Could not get the firmware list: {}\n", why).as_str());
                    self.toasts
                        .error(format!(
                            "Could not get the firmware list\n{}. You can still flash a file from this PC.",
                            why
                        ))
                        .set_duration(Some(Duration::from_secs(8)))
                        .set_closable(true);
                }
            }
        }

        Flasher::render_header(self, ctx);
        Flasher::render_main_windows(self, ctx);

        if self.config.download {
            let path: PathBuf = std::env::temp_dir().join(format!(
                "{}-{}.zip",
                submodules::sources::file_key(&self.config.version),
                self.config.int_name
            ));
            let pathlock: PathBuf = [std::env::temp_dir(), "firmware.lock".into()]
                .iter()
                .collect();
            if self.config.download_firm_notify {
                let _ = std::fs::write(pathlock, "Locked");
                self.toasts
                    .info("Downloading firmware")
                    .set_duration(None)
                    .set_closable(false);
                self.config.logs.push_str(
                    format!(
                        "PineFlash: Downloading Firmware {} {}.\n",
                        self.config.int_name, self.config.version
                    )
                    .as_str(),
                );
                let _ = std::fs::remove_file(path);
                let version = self.config.version.clone();
                let int_name = self.config.int_name.clone();
                std::thread::spawn(move || {
                    let path: PathBuf = std::env::temp_dir()
                        .join(format!("{}-{}.zip", submodules::sources::file_key(&version), int_name));
                    let pathlock: PathBuf = std::env::temp_dir().join("firmware.lock");
                    let url = submodules::sources::asset_url(&version, &format!("{}.zip", int_name));
                    if let Ok(zip) = download(&url) {
                        let _ = std::fs::write(path, zip);
                    }
                    // Always release the lock, even on failure, so the UI can't hang.
                    let _ = std::fs::remove_file(pathlock);
                });
                self.config.download_firm_notify = false
            } else if !self.config.download_firm_notify && !pathlock.exists() {
                self.toasts.dismiss_all_toasts();
                self.config.download = false;
                if path.exists() {
                    self.config.logs.push_str("PineFlash: Download Complete.\n");
                    self.toasts
                        .info("Flashing")
                        .set_duration(None)
                        .set_closable(false);
                    self.config.flash = true;
                } else {
                    self.config
                        .logs
                        .push_str("PineFlash: Firmware download failed (network error or file not on GitHub).\n");
                    self.toasts
                        .error("Firmware download failed\nCheck your internet connection and try again.")
                        .set_duration(Some(Duration::from_secs(6)))
                        .set_closable(true);
                    self.config.download_firm_notify = true;
                }
            }
        }
        if !self.config.version.contains("Select")
            && !self.config.version.contains("Custom")
            && self.config.download_metadata
        {
            let path: PathBuf = [std::env::temp_dir(), "langs.zip".into()].iter().collect();
            let pathlock: PathBuf = [std::env::temp_dir(), "langs.lock".into()].iter().collect();
            if self.config.run_once_vers {
                self.toasts
                    .info("Downloading language list")
                    .set_duration(None)
                    .set_closable(false);
                let _ = std::fs::remove_file(path);
                let _ = std::fs::write(pathlock, "Locked");
                let version = self.config.version.clone();
                std::thread::spawn(move || {
                    let path: PathBuf = std::env::temp_dir().join("langs.zip");
                    let pathlock: PathBuf = std::env::temp_dir().join("langs.lock");
                    let _ = std::fs::remove_file(&path);
                    let url = submodules::sources::asset_url(&version, "metadata.zip");
                    if let Ok(zip) = download(&url) {
                        let _ = std::fs::write(path, zip);
                    }
                    let _ = std::fs::remove_file(pathlock);
                });
                self.config.run_once_vers = false;
            } else if !pathlock.exists() {
                self.toasts.dismiss_all_toasts();
                self.config.download_metadata = false;
                match self.load_languages(&path) {
                    Ok(count) => {
                        self.config.logs.push_str(
                            format!("PineFlash: Download of Language Info Complete ({} languages).\n", count)
                                .as_str(),
                        );
                        self.toasts
                            .success("Languages ready")
                            .set_duration(Some(Duration::from_secs(3)))
                            .set_closable(false);
                    }
                    Err(e) => {
                        self.config
                            .logs
                            .push_str(format!("PineFlash: Could not load languages: {}\n", e).as_str());
                        self.toasts
                            .error("Could not download the language list\nEnglish (EN) will be used.")
                            .set_duration(Some(Duration::from_secs(5)))
                            .set_closable(true);
                        self.config.lang = "EN".to_string();
                    }
                }
            }
        }

        if self.config.flash {
            if self.config.flash_notified_count < 60 {
                self.config.flash_notified_count += 1
            } else {
                Flasher::flash(self);
            }
        }
    }
}

#[derive(Debug)]
pub enum DownloadError {
    Network(String),
    Http(u32),
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadError::Network(e) => write!(f, "network error: {}", e),
            DownloadError::Http(403) => write!(f, "HTTP 403 (GitHub rate limit? try again in an hour)"),
            DownloadError::Http(code) => write!(f, "HTTP {}", code),
        }
    }
}

/// GET a URL, following redirects. Only a 200 response counts as success.
pub fn download(url: &str) -> Result<Vec<u8>, DownloadError> {
    let net = |e: curl::Error| DownloadError::Network(e.to_string());
    let mut data = Vec::new();
    let mut handle = Easy::new();
    handle.url(url).map_err(net)?;
    handle.useragent("PineFlash").map_err(net)?;
    handle.follow_location(true).map_err(net)?;
    handle.connect_timeout(Duration::from_secs(15)).map_err(net)?;
    // Abort if the transfer stalls (below 1 byte/s for 30 s) rather than hanging forever.
    handle.low_speed_limit(1).map_err(net)?;
    handle.low_speed_time(Duration::from_secs(30)).map_err(net)?;
    {
        let mut transfer = handle.transfer();
        transfer
            .write_function(|new_data| {
                data.extend_from_slice(new_data);
                Ok(new_data.len())
            })
            .map_err(net)?;
        transfer.perform().map_err(net)?;
    }
    match handle.response_code().map_err(net)? {
        200 => Ok(data),
        code => Err(DownloadError::Http(code)),
    }
}

impl Flasher {
    /// Unpack the release's metadata.zip and fill the language list. Returns the number of languages.
    fn load_languages(&mut self, zip_path: &std::path::Path) -> Result<usize, String> {
        let mut data = Vec::new();
        File::open(zip_path)
            .and_then(|mut f| f.read_to_end(&mut data))
            .map_err(|e| format!("download failed ({})", e))?;
        let target_dir: PathBuf = std::env::temp_dir().join("metadata");
        let _ = fs::remove_dir_all(&target_dir); // don't read stale files from an older release
        zip_extract::extract(Cursor::new(data), &target_dir, false)
            .map_err(|e| format!("invalid metadata.zip ({})", e))?;
        let json_path = target_dir.join(format!("{}.json", self.config.int_name));
        self.config.json = fs::read_to_string(&json_path)
            .map_err(|e| format!("{} missing ({})", json_path.display(), e))?;
        let value: YourValue = serde_json::from_str(self.config.json.as_str())
            .map_err(|e| format!("unexpected metadata format ({})", e))?;
        let mut langs: Vec<(String, String)> = value
            .contents
            .into_iter()
            .filter(|(file, _)| !file.contains(".hex"))
            .map(|(_, l)| (l.language_name, l.language_code))
            .collect();
        langs.sort();
        self.config.fancy_names = langs.iter().map(|l| l.0.clone()).collect();
        self.config.code_names = langs.into_iter().map(|l| l.1).collect();
        Ok(self.config.code_names.len())
    }
}

/// Colours for PineFlash's flat look (hairline borders, quiet fills, one solid primary button).
pub struct Palette {
    pub page: Color32,
    pub sidebar: Color32,
    pub field: Color32,
    pub field_hover: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub code_bg: Color32,
}

pub fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            page: Color32::from_rgb(38, 38, 36),
            sidebar: Color32::from_rgb(31, 31, 30),
            field: Color32::from_rgb(38, 38, 36),
            field_hover: Color32::from_rgb(48, 48, 46),
            border: Color32::from_rgb(66, 66, 63),
            border_strong: Color32::from_rgb(96, 96, 92),
            text: Color32::from_rgb(236, 235, 230),
            muted: Color32::from_rgb(160, 158, 150),
            code_bg: Color32::from_rgb(46, 46, 44),
        }
    } else {
        Palette {
            page: Color32::from_rgb(255, 255, 255),
            sidebar: Color32::from_rgb(248, 247, 244),
            field: Color32::from_rgb(255, 255, 255),
            field_hover: Color32::from_rgb(244, 243, 239),
            border: Color32::from_rgb(222, 220, 213),
            border_strong: Color32::from_rgb(190, 188, 180),
            text: Color32::from_rgb(31, 30, 29),
            muted: Color32::from_rgb(110, 108, 102),
            code_bg: Color32::from_rgb(241, 239, 232),
        }
    }
}

/// Dark or light theme: flat surfaces, 1px hairline borders, rounded controls.
pub fn apply_theme(ctx: &Context, dark: bool) {
    let p = palette(dark);
    let accent = submodules::main_panel::GREEN;
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = p.page;
    v.window_fill = p.page;
    v.window_stroke = Stroke::new(1.0_f32, p.border);
    v.extreme_bg_color = p.code_bg;
    v.faint_bg_color = p.sidebar;
    v.override_text_color = None;
    v.selection.bg_fill = accent.gamma_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0_f32, accent);
    v.hyperlink_color = if dark { Color32::from_rgb(133, 183, 235) } else { Color32::from_rgb(24, 95, 165) };
    v.window_rounding = Rounding::same(10.0);
    v.window_shadow = egui::epaint::Shadow::NONE;
    v.popup_shadow = egui::epaint::Shadow::NONE;

    let r = Rounding::same(8.0);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.page;
    w.noninteractive.weak_bg_fill = p.page;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border); // separators, frames
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.text);
    for (state, fill, border) in [
        (&mut w.inactive, p.field, p.border_strong),
        (&mut w.hovered, p.field_hover, p.border_strong),
        (&mut w.active, p.field_hover, p.text),
        (&mut w.open, p.field_hover, p.border_strong),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = Stroke::new(1.0_f32, border);
        state.fg_stroke = Stroke::new(1.0_f32, p.text);
        state.rounding = r;
        state.expansion = 0.0;
    }
    w.noninteractive.rounding = r;
    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(12.0, 6.0);
        s.spacing.interact_size.y = 30.0;
        s.spacing.combo_width = 200.0;
    });
}

fn main() {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("PineFlash CE")
        .with_inner_size([860.0, 640.0])
        .with_min_inner_size([700.0, 480.0]);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/pine64logo.png")) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    if let Err(error) = eframe::run_native(
        "PineFlash CE",
        options,
        Box::new(|cc| {
            // This gives us image support:
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Box::new(Flasher::new(cc))
        }),
    ) {
        // No console under windows_subsystem = "windows", so show the error in a dialog.
        rfd::MessageDialog::new()
            .set_title("PineFlash CE could not start")
            .set_description(&error.to_string())
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
}

#[cfg(test)]
mod tests {
    use super::parse_versions;

    const RELEASES: &str = r#"[
        {"tag_name":"v2.25-rc1","prerelease":true,"draft":true},
        {"tag_name":"v2.24-rc1","prerelease":true,"draft":false},
        {"tag_name":"v2.23","prerelease":false,"draft":false},
        {"tag_name":"v2.23-rc4","prerelease":true,"draft":false},
        {"tag_name":"v2.22","prerelease":false,"draft":false},
        {"tag_name":"v2.21","prerelease":false,"draft":false},
        {"tag_name":"v2.20","prerelease":false,"draft":false}
    ]"#;

    #[test]
    fn stable_only_by_default() {
        let (v, pre) = parse_versions(RELEASES, false);
        assert_eq!(v, ["v2.23", "v2.22", "v2.21"]);
        assert!(pre.is_empty());
    }

    #[test]
    fn prereleases_only_when_newer_than_stable() {
        let (v, pre) = parse_versions(RELEASES, true);
        assert_eq!(v, ["v2.24-rc1", "v2.23", "v2.22", "v2.21"]);
        assert_eq!(pre, ["v2.24-rc1"]);
    }

    #[test]
    fn bad_json_gives_nothing() {
        assert!(parse_versions("No Internet", true).0.is_empty());
    }
}
