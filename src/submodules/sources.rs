//! Custom firmware sources: extra GitHub repositories whose releases are offered in the
//! Firmware list next to official IronOS (e.g. an IronOS fork).
//!
//! A release from a custom source is stored in `config.version` as `owner/repo@tag`; official
//! IronOS releases stay plain tags. A source release must look like an IronOS release: a
//! `<Iron>.zip` asset (`Pinecil.zip` / `Pinecilv2.zip`) and, for the language list, `metadata.zip`.

use std::collections::HashMap;
use std::sync::mpsc::Receiver;

use serde_json::Value;

pub const OFFICIAL: &str = "Ralim/IronOS";
/// How many releases to list per custom source.
const PER_SOURCE: usize = 5;

#[derive(Clone, Debug, PartialEq)]
pub struct SourceRelease {
    pub tag: String,
    pub prerelease: bool,
    /// Release asset names, to see which irons a release has firmware for.
    pub assets: Vec<String>,
}

impl SourceRelease {
    pub fn has_iron(&self, int_name: &str) -> bool {
        let want = format!("{}.zip", int_name);
        self.assets.iter().any(|a| a.eq_ignore_ascii_case(&want))
    }
}

#[derive(Default)]
pub struct SourcesState {
    /// Release list per repo, or the error from fetching it.
    pub releases: HashMap<String, Result<Vec<SourceRelease>, String>>,
    pub pending: Vec<(String, Receiver<Result<Vec<SourceRelease>, String>>)>,
    pub window_open: bool,
    pub new_source: String,
}

/// "owner/repo", "https://github.com/owner/repo(.git)(/releases...)" -> "owner/repo".
pub fn parse_repo(input: &str) -> Result<String, String> {
    let s = input.trim().trim_end_matches('/');
    let s = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let s = s.strip_prefix("www.").unwrap_or(s);
    let s = s.strip_prefix("github.com/").unwrap_or(s);
    let mut parts = s.split('/');
    let (Some(owner), Some(repo)) = (parts.next(), parts.next()) else {
        return Err("Enter a GitHub repository as owner/name, e.g. Ralim/IronOS".into());
    };
    let repo = repo.trim_end_matches(".git");
    let ok = |p: &str| {
        !p.is_empty()
            && p.len() <= 100
            && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            && p != "."
            && p != ".."
    };
    if !ok(owner) || !ok(repo) {
        return Err(format!("\"{}\" is not a GitHub repository name", input.trim()));
    }
    Ok(format!("{}/{}", owner, repo))
}

/// `config.version` -> (repository, tag).
pub fn split_version(version: &str) -> (String, String) {
    match version.split_once('@') {
        Some((repo, tag)) if repo.contains('/') => (repo.to_string(), tag.to_string()),
        _ => (OFFICIAL.to_string(), version.to_string()),
    }
}

pub fn is_custom(version: &str) -> bool {
    split_version(version).0 != OFFICIAL
}

/// Safe for use in a file name.
pub fn file_key(version: &str) -> String {
    version
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' })
        .collect()
}

pub fn asset_url(version: &str, asset: &str) -> String {
    let (repo, tag) = split_version(version);
    format!("https://github.com/{}/releases/download/{}/{}", repo, tag, asset)
}

/// Short label for the selected version, e.g. "v2.23" or "someone/IronOS · v2.24-x".
pub fn display(version: &str) -> String {
    let (repo, tag) = split_version(version);
    if repo == OFFICIAL {
        tag
    } else {
        format!("{} · {}", repo, tag)
    }
}

/// Parse GitHub's release list (newest first).
pub fn parse_releases(json: &str) -> Result<Vec<SourceRelease>, String> {
    let v: Value = serde_json::from_str(json).map_err(|_| "GitHub sent something that isn't a release list".to_string())?;
    let Some(list) = v.as_array() else {
        let msg = v["message"].as_str().unwrap_or("unexpected reply from GitHub");
        return Err(msg.to_string());
    };
    Ok(list
        .iter()
        .filter(|r| !r["draft"].as_bool().unwrap_or(false))
        .filter_map(|r| {
            Some(SourceRelease {
                tag: r["tag_name"].as_str()?.to_string(),
                prerelease: r["prerelease"].as_bool().unwrap_or(false),
                assets: r["assets"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x["name"].as_str().map(str::to_string)).collect())
                    .unwrap_or_default(),
            })
        })
        .collect())
}

/// Fetch a repo's releases (runs on a background thread).
pub fn fetch(repo: &str) -> Result<Vec<SourceRelease>, String> {
    let url = format!("https://api.github.com/repos/{}/releases?per_page=30", repo);
    let body = crate::download(&url).map_err(|e| match e {
        crate::DownloadError::Http(404) => "repository not found (or it is private)".to_string(),
        other => other.to_string(),
    })?;
    parse_releases(&String::from_utf8_lossy(&body))
}

/// The releases of `repo` to show for this iron: newest first, at most PER_SOURCE.
pub fn releases_for<'a>(state: &'a SourcesState, repo: &str, int_name: &str) -> Vec<&'a SourceRelease> {
    match state.releases.get(repo) {
        Some(Ok(list)) => list.iter().filter(|r| r.has_iron(int_name)).take(PER_SOURCE).collect(),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// App side: background fetching and the "Firmware sources" window
// ---------------------------------------------------------------------------

use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use eframe::egui::{self, RichText};

impl crate::Flasher {
    /// Called every frame: fetch release lists for sources that don't have one yet and
    /// collect finished fetches. Never blocks the UI.
    pub fn poll_sources(&mut self) {
        for repo in self.saved_config.sources.clone() {
            let st = &self.config.sources;
            if !st.releases.contains_key(&repo) && !st.pending.iter().any(|(r, _)| *r == repo) {
                self.fetch_source(repo);
            }
        }
        let mut done = Vec::new();
        self.config.sources.pending.retain(|(repo, rx)| match rx.try_recv() {
            Ok(r) => {
                done.push((repo.clone(), r));
                false
            }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => {
                done.push((repo.clone(), Err("the request stopped".into())));
                false
            }
        });
        for (repo, result) in done {
            match &result {
                Ok(list) => self.config.logs.push_str(
                    format!("PineFlash: Source {}: {} releases found.\n", repo, list.len()).as_str(),
                ),
                Err(e) => self.config.logs.push_str(format!("PineFlash: Source {}: {}\n", repo, e).as_str()),
            }
            self.config.sources.releases.insert(repo, result);
        }
    }

    fn fetch_source(&mut self, repo: String) {
        let (tx, rx) = std::sync::mpsc::channel();
        let r = repo.clone();
        std::thread::spawn(move || {
            let _ = tx.send(fetch(&r));
        });
        self.config.sources.releases.remove(&repo);
        self.config.sources.pending.push((repo, rx));
    }

    fn add_source(&mut self, input: &str) -> Result<String, String> {
        let repo = parse_repo(input)?;
        if repo.eq_ignore_ascii_case(OFFICIAL) {
            return Err("Official IronOS is always included.".into());
        }
        if self.saved_config.sources.iter().any(|s| s.eq_ignore_ascii_case(&repo)) {
            return Err(format!("{} is already a source.", repo));
        }
        self.saved_config.sources.push(repo.clone());
        self.save_settings();
        self.fetch_source(repo.clone());
        self.config.logs.push_str(format!("PineFlash: Added firmware source {}.\n", repo).as_str());
        Ok(repo)
    }

    fn remove_source(&mut self, repo: &str) {
        self.saved_config.sources.retain(|s| s != repo);
        self.save_settings();
        self.config.sources.releases.remove(repo);
        self.config.sources.pending.retain(|(r, _)| r != repo);
        if split_version(&self.config.version).0 == repo {
            self.config.version = "Select".to_string();
        }
        self.config.logs.push_str(format!("PineFlash: Removed firmware source {}.\n", repo).as_str());
    }

    pub fn render_sources_window(&mut self, ctx: &egui::Context) {
        if !self.config.sources.window_open {
            return;
        }
        let p = crate::palette(self.saved_config.dark_mode);
        let int_name = self.config.int_name.clone();
        let mut open = true;
        let mut remove: Option<String> = None;
        let mut refresh: Option<String> = None;
        let mut add = false;

        egui::Window::new("Firmware sources")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(480.)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_width(480.);
                ui.label(
                    RichText::new(
                        "Add a GitHub repository that publishes IronOS-style releases, such as an IronOS fork. \
                         Its newest releases appear in the Firmware list. Only add sources you trust: \
                         the firmware runs on your iron.",
                    )
                    .color(p.muted),
                );
                ui.add_space(10.);

                let row = |ui: &mut egui::Ui, repo: &str, status: RichText| {
                    ui.horizontal(|ui| {
                        ui.hyperlink_to(repo, format!("https://github.com/{}/releases", repo));
                        ui.label(status);
                    });
                };
                row(ui, OFFICIAL, RichText::new("official").size(12.5).color(p.muted));

                for repo in self.saved_config.sources.clone() {
                    let pending = self.config.sources.pending.iter().any(|(r, _)| *r == repo);
                    let status = if pending {
                        RichText::new("checking...").size(12.5).color(p.muted)
                    } else {
                        match self.config.sources.releases.get(&repo) {
                            Some(Ok(_)) => {
                                let n = releases_for(&self.config.sources, &repo, &int_name).len();
                                if n == 0 {
                                    RichText::new("no builds for this iron").size(12.5).color(super::main_panel::AMBER)
                                } else {
                                    RichText::new(format!("{} build{} for this iron", n, if n == 1 { "" } else { "s" }))
                                        .size(12.5)
                                        .color(super::main_panel::GREEN)
                                }
                            }
                            Some(Err(e)) => RichText::new(e.clone()).size(12.5).color(super::main_panel::RED),
                            None => RichText::new("").size(12.5),
                        }
                    };
                    ui.horizontal(|ui| {
                        ui.hyperlink_to(&repo, format!("https://github.com/{}/releases", repo));
                        ui.label(status);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.link("Remove").clicked() {
                                remove = Some(repo.clone());
                            }
                            if !pending && ui.link("Refresh").clicked() {
                                refresh = Some(repo.clone());
                            }
                        });
                    });
                }

                ui.add_space(10.);
                ui.separator();
                ui.add_space(6.);
                ui.horizontal(|ui| {
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.config.sources.new_source)
                            .hint_text("owner/repo or a GitHub link")
                            .desired_width(340.),
                    );
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("Add source").clicked() || enter {
                        add = true;
                    }
                });
                ui.label(
                    RichText::new("A release needs a Pinecil.zip or Pinecilv2.zip asset (and metadata.zip for languages), like official IronOS.")
                        .size(12.)
                        .color(p.muted),
                );
            });

        if add {
            let input = self.config.sources.new_source.clone();
            match self.add_source(&input) {
                Ok(repo) => {
                    self.config.sources.new_source.clear();
                    self.toasts
                        .success(format!("Source added\n{} will appear in the Firmware list.", repo))
                        .set_duration(Some(Duration::from_secs(4)));
                }
                Err(e) => {
                    self.toasts
                        .error(format!("Could not add the source\n{}", e))
                        .set_duration(Some(Duration::from_secs(5)));
                }
            }
        }
        if let Some(repo) = refresh {
            self.fetch_source(repo);
        }
        if let Some(repo) = remove {
            self.remove_source(&repo);
        }
        self.config.sources.window_open = open;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_inputs() {
        assert_eq!(parse_repo("Ralim/IronOS").unwrap(), "Ralim/IronOS");
        assert_eq!(parse_repo(" https://github.com/someone/IronOS-fork.git/ ").unwrap(), "someone/IronOS-fork");
        assert_eq!(parse_repo("github.com/a/b/releases").unwrap(), "a/b");
        assert!(parse_repo("justaname").is_err());
        assert!(parse_repo("a/b c").is_err());
        assert!(parse_repo("../etc").is_err());
    }

    #[test]
    fn versions_round_trip() {
        assert_eq!(split_version("v2.23"), (OFFICIAL.to_string(), "v2.23".to_string()));
        assert_eq!(split_version("me/IronOS@v2.24-x"), ("me/IronOS".to_string(), "v2.24-x".to_string()));
        assert!(!is_custom("v2.23"));
        assert!(is_custom("me/IronOS@v1"));
        assert_eq!(file_key("me/IronOS@v1"), "me_IronOS_v1");
        assert_eq!(
            asset_url("me/IronOS@v1", "Pinecilv2.zip"),
            "https://github.com/me/IronOS/releases/download/v1/Pinecilv2.zip"
        );
        assert_eq!(asset_url("v2.23", "metadata.zip"), "https://github.com/Ralim/IronOS/releases/download/v2.23/metadata.zip");
        assert_eq!(display("me/IronOS@v1"), "me/IronOS · v1");
    }

    /// Network test: the official repo has the layout a custom source needs.
    ///   cargo test live_fetch -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_fetch() {
        let list = fetch(OFFICIAL).unwrap();
        let state = SourcesState { releases: [(OFFICIAL.to_string(), Ok(list))].into(), ..Default::default() };
        let v2 = releases_for(&state, OFFICIAL, "Pinecilv2");
        println!("{:?}", v2.iter().map(|r| &r.tag).collect::<Vec<_>>());
        assert!(!v2.is_empty());
        assert!(fetch("GRIMREAPER35487/this-repo-does-not-exist-12345").unwrap_err().contains("not found"));
    }

    #[test]
    fn release_list() {
        let json = r#"[
            {"tag_name":"draft","draft":true,"prerelease":false,"assets":[{"name":"Pinecilv2.zip"}]},
            {"tag_name":"v2","draft":false,"prerelease":true,"assets":[{"name":"Pinecilv2.zip"},{"name":"metadata.zip"}]},
            {"tag_name":"v1","draft":false,"prerelease":false,"assets":[{"name":"Pinecil.zip"}]}
        ]"#;
        let list = parse_releases(json).unwrap();
        assert_eq!(list.len(), 2);
        assert!(list[0].prerelease && list[0].has_iron("Pinecilv2") && !list[0].has_iron("Pinecil"));
        assert!(list[1].has_iron("Pinecil"));
        assert_eq!(parse_releases(r#"{"message":"Not Found"}"#).unwrap_err(), "Not Found");
    }
}
