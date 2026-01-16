use std::borrow::Cow;
use std::path::Path;

use rootcause::Report;
use rootcause::prelude::ResultExt;
use tracing::debug_span;
use tracing::error;
use tracing::warn;
use url::Url;
use walkdir::WalkDir;

pub use crate::matchers_types::*;

impl Matcher {
    pub fn handles_host(&self, host: &str) -> bool {
        if self.name == "global" {
            return true;
        }

        for supported_host in &self.hosts {
            let mut needle = supported_host.as_str();
            if needle.starts_with("*.") {
                // This matches any subdomain
                needle = &needle[1..];

                // Check if this is a match against a wildcard subdomain
                // or a literal match with the TLD
                if host.ends_with(needle) || host == &needle[1..] {
                    return true;
                }
            } else if host == needle {
                return true;
            }
        }

        false
    }

    pub fn run_replacements(&self, url: &mut Url) -> ReplacementResult {
        // Check the path matchers first -- these may request a redirect
        if let Some(segments) = url.path_segments() {
            for segment in segments {
                for path_matcher in &self.path_matchers {
                    if segment == path_matcher.name && path_matcher.operation == ReplacementOperation::RequestRedirect {
                        return ReplacementResult::RequestRedirect;
                    }
                }
            }
        }

        // We use a Vec instead of a HashMap to keep ordering consistent.
        let mut query_pairs: Vec<(String, String)> =
            url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        let had_query_string = url.query().is_some();

        // We use a Vec here because wildcard matches may match multiple query strings
        let mut matched_names = Vec::new();
        for param_matcher in &self.param_matchers {
            let matcher_name = &param_matcher.name;

            // If it has a wildcard SUFFIX, we check if the name starts with the needle
            // and vice versa for wildcard PREFIX.
            if let Some(needle) = matcher_name.strip_suffix("*") {
                for (name, _) in &query_pairs {
                    if name.starts_with(needle) {
                        matched_names.push(Cow::Owned(name.clone()));
                    }
                }
            } else if let Some(needle) = matcher_name.strip_prefix("*") {
                for (name, _) in &query_pairs {
                    if name.ends_with(needle) {
                        matched_names.push(Cow::Owned(name.clone()));
                    }
                }
            } else if query_pairs.iter().any(|(name, _)| name == matcher_name) {
                // Simple case -- literal match
                matched_names.push(Cow::Borrowed(matcher_name))
            } else {
                // It was not a wildcard and none of the query pairs had a literal
                // match
                continue;
            };

            for matched_name in matched_names.drain(..) {
                let Some(existing_index) = query_pairs.iter().position(|(key, _)| key == matched_name.as_ref()) else {
                    warn!("BUG: failed to get replacement operation index");
                    continue;
                };

                match &param_matcher.operation {
                    ReplacementOperation::Drop => {
                        query_pairs.remove(existing_index);
                    }
                    ReplacementOperation::ReplaceWith(new_text) => {
                        query_pairs[existing_index].1 = new_text.clone();
                    }
                    ReplacementOperation::RequestRedirect => {
                        return ReplacementResult::RequestRedirect;
                    }
                }
            }
        }

        if query_pairs.is_empty() && had_query_string {
            url.set_query(None);
        } else {
            let mut new_query_pairs = url.query_pairs_mut();
            new_query_pairs.clear();
            for (k, v) in &query_pairs {
                new_query_pairs.append_pair(k, v);
            }
            new_query_pairs.finish();
        }

        if self.terminates_matching {
            ReplacementResult::Stop
        } else {
            ReplacementResult::Continue { modified: query_pairs.is_empty() && had_query_string }
        }
    }
}

pub enum ReplacementResult {
    Stop,
    Continue { modified: bool },
    RequestRedirect,
}

pub mod included_matchers {
    use super::*;
    use std::sync::LazyLock;

    include!(concat!(env!("OUT_DIR"), "/included_matchers.rs"));

    pub fn get() -> &'static [Matcher] {
        &*INCLUDED_MATCHERS
    }
}

pub fn load_matchers<P: AsRef<Path>>(path: P) -> Result<Vec<Matcher>, Report> {
    let span = debug_span!("load_matchers");
    let _enter = span.enter();

    let mut matchers = Vec::new();

    for entry in WalkDir::new(path) {
        let Ok(entry) = entry else {
            error!("failed to walkdir: {entry:?}");
            continue;
        };

        let matcher_path = entry.path();

        // We only care about TOML files
        let Some("toml") = matcher_path.extension().and_then(|ext| ext.to_str()) else {
            continue;
        };

        let Some(name) = matcher_path.file_stem() else {
            error!("filesystem path {matcher_path:?} has no file stem?");
            continue;
        };

        let data = std::fs::read(matcher_path)
            .context("failed to read matcher file")
            .attach_with(move || matcher_path.to_string_lossy().into_owned())?;

        let mut matcher: Matcher = toml::from_slice(&data)
            .context("failed to deserialize matcher")
            .attach_with(move || matcher_path.to_string_lossy().into_owned())?;

        matcher.name = name.to_string_lossy().into_owned();

        if matcher.name == "global" {
            // global matcher always comes first
            matchers.insert(0, matcher);
        } else {
            matchers.push(matcher);
        }
    }

    Ok(matchers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply_matchers(url_str: &str) -> (String, bool) {
        let mut url = Url::parse(url_str).unwrap();
        let matchers = included_matchers::get();
        let mut requested_redirect = false;

        for matcher in matchers {
            let host = url.host_str().unwrap_or("");
            if !matcher.handles_host(host) {
                continue;
            }

            match matcher.run_replacements(&mut url) {
                ReplacementResult::Stop => break,
                ReplacementResult::Continue { .. } => continue,
                ReplacementResult::RequestRedirect => {
                    requested_redirect = true;
                    break;
                }
            }
        }

        (url.to_string(), requested_redirect)
    }

    #[test]
    fn global_strips_utm_params() {
        let (result, _) = apply_matchers("https://example.com/?utm_source=twitter&utm_medium=social&keep=this");
        assert_eq!(result, "https://example.com/?keep=this");
    }

    #[test]
    fn global_strips_all_utm_variants() {
        let (result, _) = apply_matchers("https://example.com/?utm_campaign=test&utm_content=abc&utm_term=xyz");
        assert_eq!(result, "https://example.com/");
    }

    #[test]
    fn global_preserves_non_utm_params() {
        let (result, _) = apply_matchers("https://example.com/?foo=bar&baz=qux");
        assert_eq!(result, "https://example.com/?foo=bar&baz=qux");
    }

    #[test]
    fn reddit_strips_share_id() {
        let (result, _) = apply_matchers("https://www.reddit.com/r/rust/comments/abc123?share_id=xyz789&other=keep");
        assert_eq!(result, "https://www.reddit.com/r/rust/comments/abc123?other=keep");
    }

    #[test]
    fn reddit_requests_redirect_on_s_path() {
        let (_, requested_redirect) = apply_matchers("https://www.reddit.com/r/rust/s/abc123");
        assert!(requested_redirect);
    }

    #[test]
    fn reddit_subdomain_matching() {
        let (result, _) = apply_matchers("https://old.reddit.com/r/rust?share_id=abc");
        assert_eq!(result, "https://old.reddit.com/r/rust");
    }

    #[test]
    fn reddit_and_global_combined() {
        let (result, _) = apply_matchers("https://www.reddit.com/r/rust?utm_source=share&share_id=abc&keep=this");
        assert_eq!(result, "https://www.reddit.com/r/rust?keep=this");
    }

    #[test]
    fn removes_trailing_question_mark() {
        let (result, _) = apply_matchers("https://example.com/?utm_source=twitter");
        assert_eq!(result, "https://example.com/");
    }

    #[test]
    fn non_matching_host_unchanged() {
        let (result, _) = apply_matchers("https://notreddit.com/?share_id=abc");
        // share_id should remain since notreddit.com doesn't match *.reddit.com
        assert_eq!(result, "https://notreddit.com/?share_id=abc");
    }

    #[test]
    fn youtube_strips_si_param() {
        let (result, _) = apply_matchers("https://www.youtube.com/watch?v=dQw4w9WgXcQ&si=abc123");
        assert_eq!(result, "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
    }

    #[test]
    fn youtube_shortlink_strips_si_param() {
        let (result, _) = apply_matchers("https://youtu.be/dQw4w9WgXcQ?si=abc123");
        assert_eq!(result, "https://youtu.be/dQw4w9WgXcQ");
    }

    #[test]
    fn youtube_music_subdomain() {
        let (result, _) = apply_matchers("https://music.youtube.com/watch?v=abc&si=tracking");
        assert_eq!(result, "https://music.youtube.com/watch?v=abc");
    }

    #[test]
    fn spotify_strips_si_param() {
        let (result, _) = apply_matchers("https://open.spotify.com/track/abc123?si=xyz789");
        assert_eq!(result, "https://open.spotify.com/track/abc123");
    }

    #[test]
    fn spotify_non_matching_subdomain() {
        let (result, _) = apply_matchers("https://accounts.spotify.com/login?si=abc");
        assert_eq!(result, "https://accounts.spotify.com/login?si=abc");
    }
}
