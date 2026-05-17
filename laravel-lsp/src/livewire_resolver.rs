//! Helpers for locating the PHP source backing a Blade view's Livewire / Volt component.
//!
//! Three patterns are supported:
//!   - Classic Livewire — handled by `view_path_to_livewire_class_path` in `main.rs`
//!     (it depends on Backend-level project root knowledge)
//!   - Volt MFC — sibling `.php` file containing `new class extends Component`
//!   - Volt SFC — inline `new class extends Component` inside the `.blade.php` itself
//!
//! These pure path-based helpers are factored into the library so they can be
//! exercised from unit tests without spinning up a Backend.

use std::path::{Path, PathBuf};

/// Source flavor for a resolved Livewire / Volt component.
/// Controls whether downstream resolvers apply Volt-only behaviors such as
/// `mount()` parameter promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentKind {
    /// Standalone Livewire class under `app/Livewire/` or `app/Http/Livewire/`.
    Classic,
    /// Volt MFC sibling or Volt SFC inline anonymous class.
    Volt,
}

/// If the blade file has a sibling `.php` with the same stem and that sibling
/// contains a Volt anonymous-class signature, return the sibling path.
/// Returns `None` if there is no sibling, the sibling is unreadable, or the
/// sibling exists but doesn't carry a Volt signature.
pub fn volt_mfc_sibling(blade_path: &Path) -> Option<PathBuf> {
    let name = blade_path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".blade.php")?;
    let sibling = blade_path.with_file_name(format!("{}.php", stem));
    if !sibling.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&sibling).ok()?;
    if crate::php_class::detect_inline_volt_class(&content) {
        Some(sibling)
    } else {
        None
    }
}

/// True when the blade file at `blade_path` contains an inline Volt
/// `new class extends Component` declaration (Volt SFC pattern).
pub fn blade_contains_inline_volt_class(blade_path: &Path) -> bool {
    std::fs::read_to_string(blade_path)
        .ok()
        .map(|content| crate::php_class::detect_inline_volt_class(&content))
        .unwrap_or(false)
}
