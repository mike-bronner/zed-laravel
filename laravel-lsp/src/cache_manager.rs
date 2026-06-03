//! Smart caching for Laravel LSP
//!
//! Tracks file mtimes to avoid unnecessary rescanning.
//! Caches middleware, bindings, and component data to disk.
//!
//! Cache location follows XDG Base Directory Specification:
//! - Linux: ~/.cache/laravel-lsp/{project-hash}/cache.json
//! - macOS: ~/Library/Caches/org.mike-bronner.laravel-lsp/{project-hash}/cache.json
//! - Windows: %LOCALAPPDATA%\mike-bronner\laravel-lsp\cache\{project-hash}\cache.json

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tracing::{debug, info, warn};

/// Current cache version - increment when cache format changes
/// v2: Split 'file' into 'class_file' (for existence) and 'source_file' (for navigation)
/// v3: Moved cache to XDG-compliant location
/// v4: Middleware alias source_line corrected from 0-based to 1-based
///     (matches bindings + the goto-def consumer's expectation; was
///     causing goto-def for any non-first bulk-array alias to land one
///     line above the actual alias). v3 caches still parse but encode
///     the wrong line — drop them on read so rebuilt entries get the
///     correct line.
/// v5: CachedLaravelConfig now persists the service-provider namespace maps
///     (view namespaces, component namespaces, and anonymous-component
///     paths/namespaces). v4 caches lack them, so namespaced components would
///     resolve as "not found" until a provider edit forced a rebuild — drop
///     v4 on read so the namespace maps are indexed and cached up front.
const CACHE_VERSION: u32 = 5;

/// Get the XDG-compliant cache directory for a project
///
/// Returns platform-specific cache directory:
/// - Linux: ~/.cache/laravel-lsp/{project-hash}/
/// - macOS: ~/Library/Caches/org.mike-bronner.laravel-lsp/{project-hash}/
/// - Windows: %LOCALAPPDATA%\mike-bronner\laravel-lsp\cache\{project-hash}\
fn get_cache_dir(project_root: &Path) -> Option<PathBuf> {
    // Get platform-specific cache directory
    let proj_dirs = ProjectDirs::from("org", "mike-bronner", "laravel-lsp")?;
    let cache_base = proj_dirs.cache_dir();

    // Create unique hash for this project based on its absolute path
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    let project_hash = format!("{:x}", hasher.finish());

    Some(cache_base.join(project_hash))
}

/// Get the cache file path for a project
fn get_cache_file(project_root: &Path) -> Option<PathBuf> {
    get_cache_dir(project_root).map(|dir| dir.join("cache.json"))
}

/// Stored file modification time
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileMtime {
    pub mtime_secs: u64,
    pub mtime_nanos: u32,
}

impl FileMtime {
    /// Create from SystemTime
    pub fn from_system_time(time: SystemTime) -> Self {
        match time.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(duration) => FileMtime {
                mtime_secs: duration.as_secs(),
                mtime_nanos: duration.subsec_nanos(),
            },
            Err(_) => FileMtime {
                mtime_secs: 0,
                mtime_nanos: 0,
            },
        }
    }

    /// Get mtime from a file path
    pub fn from_path(path: &Path) -> Option<Self> {
        fs::metadata(path)
            .ok()
            .and_then(|m| m.modified().ok())
            .map(Self::from_system_time)
    }
}

/// A cached middleware entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiddlewareEntry {
    pub class: String,
    /// Path to the middleware class file (for existence checking)
    pub class_file: Option<String>,
    /// Path to the source file where the alias is declared (for navigation)
    pub source_file: Option<String>,
    /// Line number in source_file
    pub line: u32,
}

/// A cached binding entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindingEntry {
    pub class: String,
    pub binding_type: String,
    /// Path to the concrete class file (for existence checking)
    pub class_file: Option<String>,
    /// Path to the source file where the binding is declared (for navigation)
    pub source_file: Option<String>,
    /// Line number in source_file
    pub line: u32,
}

/// Results from scanning a directory
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanResult {
    pub middleware: HashMap<String, MiddlewareEntry>,
    pub bindings: HashMap<String, BindingEntry>,
}

impl ScanResult {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.middleware.is_empty() && self.bindings.is_empty()
    }

    pub fn merge(&mut self, other: ScanResult) {
        self.middleware.extend(other.middleware);
        self.bindings.extend(other.bindings);
    }
}

/// Results from scanning node_modules
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeModulesScan {
    pub flux_components: Vec<String>,
    pub livewire_volt_components: Vec<String>,
}

/// Cached Laravel configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CachedLaravelConfig {
    pub root: PathBuf,
    pub view_paths: Vec<PathBuf>,
    pub component_paths: Vec<(String, PathBuf)>,
    pub livewire_path: Option<PathBuf>,
    pub has_livewire: bool,
    /// Package view namespaces from loadViewsFrom() — prefix → view path.
    #[serde(default)]
    pub view_namespaces: HashMap<String, PathBuf>,
    /// Class-based component namespaces from Blade::componentNamespace() —
    /// prefix → PHP namespace.
    #[serde(default)]
    pub component_namespaces: HashMap<String, String>,
    /// Anonymous component paths from Blade::anonymousComponentPath() —
    /// prefix → absolute components directory.
    #[serde(default)]
    pub anonymous_component_paths: HashMap<String, PathBuf>,
    /// Anonymous component namespaces from Blade::anonymousComponentNamespace()
    /// — prefix → view-relative directory.
    #[serde(default)]
    pub anonymous_component_namespaces: HashMap<String, String>,
}

/// Cached environment variables
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CachedEnvVars {
    pub variables: HashMap<String, String>,
}

/// The full cache structure stored on disk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspCache {
    /// Cache version for compatibility checking
    pub version: u32,
    /// Project root path
    pub project_root: PathBuf,
    /// Watched file mtimes
    pub watched_files: HashMap<String, FileMtime>,
    /// Vendor directory scan results (framework + packages)
    pub vendor_scan: ScanResult,
    /// App directory scan results (app providers + bootstrap)
    pub app_scan: ScanResult,
    /// Node modules scan results
    pub node_modules_scan: NodeModulesScan,
    /// Laravel configuration (view paths, component paths, etc.)
    pub laravel_config: Option<CachedLaravelConfig>,
    /// Environment variables from .env files
    pub env_vars: Option<CachedEnvVars>,
}

impl LspCache {
    fn new(project_root: PathBuf) -> Self {
        Self {
            version: CACHE_VERSION,
            project_root,
            watched_files: HashMap::new(),
            vendor_scan: ScanResult::default(),
            app_scan: ScanResult::default(),
            node_modules_scan: NodeModulesScan::default(),
            laravel_config: None,
            env_vars: None,
        }
    }
}

/// Watch file types that trigger rescans
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RescanType {
    /// Rescan vendor/ directory (triggered by composer.lock)
    Vendor,
    /// Rescan app/Providers and bootstrap/app.php
    App,
    /// Rescan node_modules (triggered by package lock files)
    NodeModules,
}

/// Manages the LSP cache
pub struct CacheManager {
    /// Path to the cache file (XDG-compliant location)
    cache_path: Option<PathBuf>,
    /// The loaded cache (None if not loaded or invalid)
    cache: Option<LspCache>,
    /// Project root
    project_root: PathBuf,
}

impl CacheManager {
    /// Load cache from disk for the given project root
    ///
    /// Loads cache from XDG-compliant location.
    pub fn load(project_root: &Path) -> Self {
        let cache_path = get_cache_file(project_root);
        let mut manager = CacheManager {
            cache_path: cache_path.clone(),
            cache: None,
            project_root: project_root.to_path_buf(),
        };

        // Try to load from XDG location
        if let Some(ref path) = cache_path {
            if path.exists() {
                match fs::read_to_string(path) {
                    Ok(content) => match serde_json::from_str::<LspCache>(&content) {
                        Ok(cache) => {
                            // Validate cache
                            if cache.version != CACHE_VERSION {
                                info!(
                                    "Cache version mismatch (got {}, expected {}), will rescan",
                                    cache.version, CACHE_VERSION
                                );
                            } else if cache.project_root != project_root {
                                info!("Cache project root mismatch, will rescan");
                            } else {
                                info!(
                                    "Loaded cache from {:?}: {} middleware, {} bindings",
                                    path,
                                    cache.vendor_scan.middleware.len()
                                        + cache.app_scan.middleware.len(),
                                    cache.vendor_scan.bindings.len()
                                        + cache.app_scan.bindings.len()
                                );
                                manager.cache = Some(cache);
                            }
                        }
                        Err(e) => {
                            warn!("Failed to parse cache file: {}", e);
                        }
                    },
                    Err(e) => {
                        debug!("Failed to read cache file: {}", e);
                    }
                }
            } else {
                debug!("No cache file found at {:?}", path);
            }
        } else {
            warn!("Could not determine XDG cache directory, caching disabled");
        }

        manager
    }

    /// Save cache to disk (XDG-compliant location)
    pub fn save(&self) -> Result<()> {
        let cache_path = match &self.cache_path {
            Some(p) => p,
            None => {
                debug!("No cache path available, skipping save");
                return Ok(());
            }
        };

        if let Some(ref cache) = self.cache {
            // Create cache directory if needed
            if let Some(parent) = cache_path.parent() {
                fs::create_dir_all(parent).context("Failed to create cache directory")?;
            }

            let content =
                serde_json::to_string_pretty(cache).context("Failed to serialize cache")?;

            fs::write(cache_path, content).context("Failed to write cache file")?;

            info!("Saved cache to {:?}", cache_path);
        }
        Ok(())
    }

    /// Get the cache file path (for debugging/testing)
    pub fn cache_path(&self) -> Option<&Path> {
        self.cache_path.as_deref()
    }

    /// Check if a watch file has changed and needs rescanning
    pub fn needs_rescan(&self, watch_file: &str) -> bool {
        let cache = match &self.cache {
            Some(c) => c,
            None => return true, // No cache = needs rescan
        };

        let full_path = self.project_root.join(watch_file);

        // Get current mtime
        let current_mtime = match FileMtime::from_path(&full_path) {
            Some(m) => m,
            None => {
                // File doesn't exist - if we had it cached, something changed
                return cache.watched_files.contains_key(watch_file);
            }
        };

        // Compare with cached mtime
        match cache.watched_files.get(watch_file) {
            Some(cached_mtime) => *cached_mtime != current_mtime,
            None => true, // Not in cache = needs rescan
        }
    }

    /// Check if any file matching a glob pattern needs rescanning
    pub fn needs_rescan_glob(&self, pattern: &str) -> bool {
        let full_pattern = self.project_root.join(pattern);
        let pattern_str = full_pattern.to_string_lossy();

        match glob::glob(&pattern_str) {
            Ok(paths) => {
                for entry in paths.flatten() {
                    let relative = entry
                        .strip_prefix(&self.project_root)
                        .unwrap_or(&entry)
                        .to_string_lossy()
                        .to_string();

                    if self.needs_rescan(&relative) {
                        return true;
                    }
                }
                false
            }
            Err(_) => true, // Invalid pattern = assume needs rescan
        }
    }

    /// Update the mtime for a watch file
    pub fn update_mtime(&mut self, watch_file: &str) {
        let full_path = self.project_root.join(watch_file);

        if let Some(mtime) = FileMtime::from_path(&full_path) {
            self.ensure_cache();
            if let Some(ref mut cache) = self.cache {
                cache.watched_files.insert(watch_file.to_string(), mtime);
            }
        }
    }

    /// Update mtimes for all files matching a glob pattern
    pub fn update_mtime_glob(&mut self, pattern: &str) {
        let full_pattern = self.project_root.join(pattern);
        let pattern_str = full_pattern.to_string_lossy();

        if let Ok(paths) = glob::glob(&pattern_str) {
            for entry in paths.flatten() {
                if let Some(mtime) = FileMtime::from_path(&entry) {
                    let relative = entry
                        .strip_prefix(&self.project_root)
                        .unwrap_or(&entry)
                        .to_string_lossy()
                        .to_string();

                    self.ensure_cache();
                    if let Some(ref mut cache) = self.cache {
                        cache.watched_files.insert(relative, mtime);
                    }
                }
            }
        }
    }

    /// Ensure cache is initialized
    fn ensure_cache(&mut self) {
        if self.cache.is_none() {
            self.cache = Some(LspCache::new(self.project_root.clone()));
        }
    }

    /// Get all cached middleware (vendor + app merged)
    pub fn get_all_middleware(&self) -> HashMap<String, MiddlewareEntry> {
        let mut result = HashMap::new();

        if let Some(ref cache) = self.cache {
            // First add vendor (lower priority)
            result.extend(cache.vendor_scan.middleware.clone());
            // Then add app (higher priority, overwrites vendor)
            result.extend(cache.app_scan.middleware.clone());
        }

        result
    }

    /// Get all cached bindings (vendor + app merged)
    pub fn get_all_bindings(&self) -> HashMap<String, BindingEntry> {
        let mut result = HashMap::new();

        if let Some(ref cache) = self.cache {
            // First add vendor (lower priority)
            result.extend(cache.vendor_scan.bindings.clone());
            // Then add app (higher priority, overwrites vendor)
            result.extend(cache.app_scan.bindings.clone());
        }

        result
    }

    /// Get cached vendor scan result
    pub fn get_vendor_scan(&self) -> Option<&ScanResult> {
        self.cache.as_ref().map(|c| &c.vendor_scan)
    }

    /// Get cached app scan result
    pub fn get_app_scan(&self) -> Option<&ScanResult> {
        self.cache.as_ref().map(|c| &c.app_scan)
    }

    /// Get cached node_modules scan result
    pub fn get_node_modules_scan(&self) -> Option<&NodeModulesScan> {
        self.cache.as_ref().map(|c| &c.node_modules_scan)
    }

    /// Set vendor scan results
    pub fn set_vendor_scan(&mut self, result: ScanResult) {
        self.ensure_cache();
        if let Some(ref mut cache) = self.cache {
            cache.vendor_scan = result;
        }
    }

    /// Set app scan results
    pub fn set_app_scan(&mut self, result: ScanResult) {
        self.ensure_cache();
        if let Some(ref mut cache) = self.cache {
            cache.app_scan = result;
        }
    }

    /// Set node_modules scan results
    pub fn set_node_modules_scan(&mut self, result: NodeModulesScan) {
        self.ensure_cache();
        if let Some(ref mut cache) = self.cache {
            cache.node_modules_scan = result;
        }
    }

    /// Get cached Laravel config
    pub fn get_laravel_config(&self) -> Option<&CachedLaravelConfig> {
        self.cache.as_ref().and_then(|c| c.laravel_config.as_ref())
    }

    /// Set Laravel config
    pub fn set_laravel_config(&mut self, config: CachedLaravelConfig) {
        self.ensure_cache();
        if let Some(ref mut cache) = self.cache {
            cache.laravel_config = Some(config);
        }
    }

    /// Get cached env variables
    pub fn get_env_vars(&self) -> Option<&CachedEnvVars> {
        self.cache.as_ref().and_then(|c| c.env_vars.as_ref())
    }

    /// Set env variables
    pub fn set_env_vars(&mut self, vars: CachedEnvVars) {
        self.ensure_cache();
        if let Some(ref mut cache) = self.cache {
            cache.env_vars = Some(vars);
        }
    }

    /// Check if cache has any data
    pub fn has_cached_data(&self) -> bool {
        self.cache
            .as_ref()
            .map(|c| {
                !c.vendor_scan.is_empty() || !c.app_scan.is_empty() || c.laravel_config.is_some()
            })
            .unwrap_or(false)
    }

    /// Determine which rescan types are needed based on file changes
    pub fn get_needed_rescans(&self) -> Vec<RescanType> {
        let mut needed = Vec::new();

        // Check composer.lock for vendor rescan
        if self.needs_rescan("composer.lock") {
            needed.push(RescanType::Vendor);
        }

        // Check app providers and bootstrap for app rescan
        if self.needs_rescan("bootstrap/app.php") || self.needs_rescan_glob("app/Providers/*.php") {
            needed.push(RescanType::App);
        }

        // Check package lock files for node_modules rescan
        if self.needs_rescan("package-lock.json")
            || self.needs_rescan("yarn.lock")
            || self.needs_rescan("pnpm-lock.yaml")
        {
            needed.push(RescanType::NodeModules);
        }

        needed
    }

    /// Invalidate a specific rescan type
    pub fn invalidate(&mut self, rescan_type: RescanType) {
        if let Some(ref mut cache) = self.cache {
            match rescan_type {
                RescanType::Vendor => {
                    cache.vendor_scan = ScanResult::default();
                    cache.watched_files.remove("composer.lock");
                }
                RescanType::App => {
                    cache.app_scan = ScanResult::default();
                    cache.watched_files.remove("bootstrap/app.php");
                    // Remove all app/Providers/*.php entries
                    cache
                        .watched_files
                        .retain(|k, _| !k.starts_with("app/Providers/"));
                }
                RescanType::NodeModules => {
                    cache.node_modules_scan = NodeModulesScan::default();
                    cache.watched_files.remove("package-lock.json");
                    cache.watched_files.remove("yarn.lock");
                    cache.watched_files.remove("pnpm-lock.yaml");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
