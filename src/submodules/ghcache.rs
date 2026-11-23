//! GitHub API requests with an on-disk cache.
//!
//! Without signing in, GitHub allows 60 API requests per hour per internet connection. To stay
//! well inside that and to keep working when it's used up:
//! * every reply is saved in ~/PineFlash/cache together with its ETag;
//! * the next request sends `If-None-Match`, and GitHub answers "304 Not Modified", which does
//!   not count against the limit, when nothing changed;
//! * if GitHub refuses (rate limit, outage, no internet), the saved reply is used instead.
//!
//! Only the API (release lists) is limited; file downloads from github.com/.../releases/download
//! are not, so they keep using plain `crate::download`.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use curl::easy::{Easy, List};

/// How fresh a reply is.
#[derive(Clone, Debug, PartialEq)]
pub enum Freshness {
    /// Downloaded just now.
    Live,
    /// GitHub confirmed the saved copy is current (free: doesn't use up the hourly limit).
    Unchanged,
    /// GitHub couldn't be asked; this is the saved copy. The string says why.
    Saved(String),
}

pub fn cache_dir() -> PathBuf {
    simple_home_dir::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("PineFlash")
        .join("cache")
}

fn safe_key(key: &str) -> String {
    key.chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' })
        .collect()
}

struct Reply {
    status: u32,
    body: Vec<u8>,
    etag: Option<String>,
    rate_remaining: Option<u64>,
    rate_reset: Option<u64>,
}

fn request(url: &str, etag: Option<&str>) -> Result<Reply, String> {
    let net = |e: curl::Error| format!("network error: {}", e);
    let mut handle = Easy::new();
    handle.url(url).map_err(net)?;
    handle.useragent("PineFlash-CE").map_err(net)?;
    handle.follow_location(true).map_err(net)?;
    handle.connect_timeout(Duration::from_secs(15)).map_err(net)?;
    handle.low_speed_limit(1).map_err(net)?;
    handle.low_speed_time(Duration::from_secs(30)).map_err(net)?;
    let mut headers = List::new();
    headers.append("Accept: application/vnd.github+json").map_err(net)?;
    if let Some(tag) = etag {
        headers.append(&format!("If-None-Match: {}", tag)).map_err(net)?;
    }
    handle.http_headers(headers).map_err(net)?;

    let mut body = Vec::new();
    let mut header_lines: Vec<String> = Vec::new();
    {
        let mut t = handle.transfer();
        t.write_function(|d| {
            body.extend_from_slice(d);
            Ok(d.len())
        })
        .map_err(net)?;
        t.header_function(|h| {
            header_lines.push(String::from_utf8_lossy(h).trim().to_string());
            true
        })
        .map_err(net)?;
        t.perform().map_err(net)?;
    }
    let status = handle.response_code().map_err(net)?;
    let header = |name: &str| {
        header_lines.iter().rev().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_string())
        })
    };
    Ok(Reply {
        status,
        body,
        etag: header("etag"),
        rate_remaining: header("x-ratelimit-remaining").and_then(|v| v.parse().ok()),
        rate_reset: header("x-ratelimit-reset").and_then(|v| v.parse().ok()),
    })
}

fn rate_limit_message(reset: Option<u64>) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    match reset {
        Some(r) if r > now => {
            let mins = (r - now + 59) / 60;
            format!("GitHub's hourly request limit is used up (resets in {} min)", mins)
        }
        _ => "GitHub's hourly request limit is used up".to_string(),
    }
}

/// GET a GitHub API URL. `key` names the cache file. Returns the JSON text and how fresh it is;
/// an error only when GitHub can't be asked and nothing is saved, or the resource doesn't exist.
pub fn get_json(url: &str, key: &str) -> Result<(String, Freshness), String> {
    let dir = cache_dir();
    let body_path = dir.join(format!("{}.json", safe_key(key)));
    let etag_path = dir.join(format!("{}.etag", safe_key(key)));
    let saved = std::fs::read_to_string(&body_path).ok().filter(|s| !s.trim().is_empty());
    let etag = saved.as_ref().and_then(|_| std::fs::read_to_string(&etag_path).ok());

    let fallback = |why: String| match &saved {
        Some(s) => Ok((s.clone(), Freshness::Saved(why))),
        None => Err(why),
    };

    let reply = match request(url, etag.as_deref().map(str::trim)) {
        Ok(r) => r,
        Err(e) => return fallback(e),
    };
    match reply.status {
        200 => {
            let text = String::from_utf8_lossy(&reply.body).into_owned();
            if std::fs::create_dir_all(&dir).is_ok() {
                // Write then rename, so a crash never leaves half a file.
                let tmp = body_path.with_extension("json.part");
                if std::fs::write(&tmp, &text).is_ok() && std::fs::rename(&tmp, &body_path).is_ok() {
                    match &reply.etag {
                        Some(tag) => {
                            let _ = std::fs::write(&etag_path, tag);
                        }
                        None => {
                            let _ = std::fs::remove_file(&etag_path);
                        }
                    }
                }
            }
            Ok((text, Freshness::Live))
        }
        304 => fallback("GitHub said nothing changed, but the saved copy is missing".into())
            .map(|(s, _)| (s, Freshness::Unchanged)),
        404 => Err("not found on GitHub (or it is private)".into()),
        403 | 429 if reply.rate_remaining == Some(0) || reply.status == 429 => {
            fallback(rate_limit_message(reply.rate_reset))
        }
        code => fallback(format!("GitHub answered HTTP {}", code)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_file_safe() {
        assert_eq!(safe_key("source-me/IronOS"), "source-me_IronOS");
        assert_eq!(safe_key("ironos-releases"), "ironos-releases");
    }

    #[test]
    fn rate_limit_text() {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert!(rate_limit_message(Some(now + 600)).contains("resets in 10 min"));
        assert!(rate_limit_message(None).contains("used up"));
    }

    /// Network test: second request is answered "unchanged" (or from the saved copy if the
    /// hourly limit is used up), never an error.
    ///   cargo test live_cache -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_cache() {
        let url = "https://api.github.com/repos/Ralim/IronOS/releases?per_page=2";
        let first = get_json(url, "test-live-cache");
        println!("first: {:?}", first.as_ref().map(|(_, f)| f));
        let (_, second) = get_json(url, "test-live-cache").unwrap();
        println!("second: {:?}", second);
        assert_ne!(second, Freshness::Live, "expected 304 or saved copy");
        let _ = std::fs::remove_file(cache_dir().join("test-live-cache.json"));
        let _ = std::fs::remove_file(cache_dir().join("test-live-cache.etag"));
    }
}
