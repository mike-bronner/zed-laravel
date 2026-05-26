//! Detect the installed major version of Livewire from `composer.lock`.
//!
//! Phase 3 rename routes differently for Livewire 3 vs 4 — v4 introduced
//! single-file and multi-file component formats with the `⚡` emoji prefix
//! and configurable `component_locations`. The resolver needs to know which
//! shape the project is on to pick the right discovery defaults and emit
//! the right file layout when creating new files during cross-dir renames.
//!
//! The parser is text-based — `composer.lock` is large JSON but we only
//! need one field, and pulling the whole document through `serde_json` would
//! be wasteful. Matches the style of `parse_composer_json` in `salsa_impl`.

/// Resolved major version of `livewire/livewire`.
///
/// `Unknown` covers both "Livewire isn't installed" and "the version string
/// couldn't be parsed" — callers treat both the same way (fall back to v3
/// defaults, which are the more conservative shape).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LivewireVersion {
    V3,
    V4,
    Unknown,
}

/// Scan a `composer.lock` JSON string for the `livewire/livewire` package
/// entry and return its detected major version.
pub fn detect_from_composer_lock(json: &str) -> LivewireVersion {
    // Each package entry in composer.lock looks roughly like:
    //   { "name": "livewire/livewire", "version": "v4.0.3", ... }
    // The `name` and `version` keys sit close together within the same JSON
    // object, so a small lookahead window after the name match is enough.
    let Some(name_pos) = find_livewire_name(json) else {
        return LivewireVersion::Unknown;
    };

    let window_end = name_pos.saturating_add(LOOKAHEAD_BYTES).min(json.len());
    let window = &json[name_pos..window_end];
    let Some(version_str) = extract_first_version_value(window) else {
        return LivewireVersion::Unknown;
    };

    match major_segment(&version_str) {
        "3" => LivewireVersion::V3,
        "4" => LivewireVersion::V4,
        _ => LivewireVersion::Unknown,
    }
}

const LOOKAHEAD_BYTES: usize = 500;

fn find_livewire_name(json: &str) -> Option<usize> {
    // Tolerate both compact and pretty-printed JSON spacing variations.
    for needle in [
        "\"name\": \"livewire/livewire\"",
        "\"name\":\"livewire/livewire\"",
    ] {
        if let Some(pos) = json.find(needle) {
            return Some(pos);
        }
    }
    None
}

fn extract_first_version_value(window: &str) -> Option<String> {
    let key_pos = window
        .find("\"version\":")
        .or_else(|| window.find("\"version\" :"))?;
    let after_key = &window[key_pos..];
    let after_colon = after_key.split_once(':')?.1;
    let trimmed = after_colon.trim_start();
    let rest = trimmed.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn major_segment(version: &str) -> &str {
    // Strip an optional leading `v` (composer publishes both `v4.0.3` and
    // `4.0.3` shapes depending on the package), then take the first dotted
    // segment.
    let stripped = version.strip_prefix('v').unwrap_or(version);
    stripped.split('.').next().unwrap_or("")
}

#[cfg(test)]
mod tests;
