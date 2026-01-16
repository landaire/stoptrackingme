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
                needle = &needle[2..];

                if host.ends_with(needle) {
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

        let mut matched_names = Vec::new();
        for param_matcher in &self.param_matchers {
            let matcher_name = &param_matcher.name;

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
                matched_names.push(Cow::Borrowed(matcher_name))
            } else {
                // No matches
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

        let mut new_query_pairs = url.query_pairs_mut();
        new_query_pairs.clear();
        for (k, v) in &query_pairs {
            new_query_pairs.append_pair(k, v);
        }
        new_query_pairs.finish();

        if self.terminates_matching { ReplacementResult::Stop } else { ReplacementResult::Continue }
    }
}

pub enum ReplacementResult {
    Stop,
    Continue,
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
