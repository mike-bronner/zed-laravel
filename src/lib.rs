use std::fs;
use zed_extension_api::{self as zed, Result};

/// Extension version - used for versioned binary directory
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The main struct for our Laravel extension
struct LaravelExtension {
    /// Cached path to the language server binary
    cached_binary_path: Option<String>,
}

impl zed::Extension for LaravelExtension {
    fn new() -> Self {
        LaravelExtension {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let binary_path = self.language_server_binary_path(worktree)?;

        Ok(zed::Command {
            command: binary_path,
            args: vec![],
            env: worktree.shell_env(),
        })
    }

    fn language_server_initialization_options(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        _worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        Ok(None)
    }
}

impl LaravelExtension {
    /// Get or download the language server binary
    ///
    /// Search order:
    /// 1. Check cached path (verify still exists)
    /// 2. Check versioned extension directory (laravel-lsp-{VERSION}/)
    /// 3. Try system PATH via worktree.which()
    /// 4. Download from GitHub releases
    fn language_server_binary_path(&mut self, worktree: &zed::Worktree) -> Result<String> {
        // Step 1: Check cached path
        if let Some(cached_path) = &self.cached_binary_path {
            if fs::metadata(cached_path).is_ok() {
                return Ok(cached_path.clone());
            }
        }

        let binary_name = Self::get_platform_binary_name();
        let version_dir = format!("laravel-lsp-{}", VERSION);
        let binary_path = format!("{}/{}", version_dir, binary_name);

        // Step 2: Check versioned extension directory
        if fs::metadata(&binary_path).is_ok() {
            self.cached_binary_path = Some(binary_path.clone());
            return Ok(binary_path);
        }

        // Step 3: Try system PATH
        if let Some(path) = worktree.which(&binary_name) {
            self.cached_binary_path = Some(path.clone());
            return Ok(path);
        }

        // Also try generic name in PATH
        if let Some(path) = worktree.which("laravel-lsp") {
            self.cached_binary_path = Some(path.clone());
            return Ok(path);
        }

        // Step 4: Download from GitHub releases
        let downloaded_path = self.download_binary(&binary_name, &version_dir)?;
        self.cached_binary_path = Some(downloaded_path.clone());
        Ok(downloaded_path)
    }

    /// Download the binary from GitHub releases
    fn download_binary(&self, binary_name: &str, version_dir: &str) -> Result<String> {
        let binary_path = format!("{}/{}", version_dir, binary_name);

        // Check if already downloaded
        if fs::metadata(&binary_path).is_ok() {
            return Ok(binary_path);
        }

        let (os, _arch) = zed::current_platform();
        let archive_ext = match os {
            zed::Os::Windows => "zip",
            _ => "tar.gz",
        };
        let archive_name = format!("{}.{}", binary_name, archive_ext);

        let release_url = format!(
            "https://github.com/GeneaLabs/zed-laravel/releases/download/{}/{}",
            VERSION, archive_name
        );

        let file_type = match os {
            zed::Os::Windows => zed::DownloadedFileType::Zip,
            _ => zed::DownloadedFileType::GzipTar,
        };

        // Download and extract
        zed::download_file(&release_url, version_dir, file_type)
            .map_err(|e| format!("Failed to download Laravel LSP binary: {}", e))?;

        // Verify extraction succeeded
        if fs::metadata(&binary_path).is_err() {
            return Err(format!(
                "Binary not found after extraction. Expected at: {}",
                binary_path
            ));
        }

        // Make executable on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = fs::metadata(&binary_path) {
                let mut perms = metadata.permissions();
                perms.set_mode(0o755);
                let _ = fs::set_permissions(&binary_path, perms);
            }
        }

        Ok(binary_path)
    }

    /// Get platform-specific binary name
    fn get_platform_binary_name() -> String {
        let (os, arch) = zed::current_platform();
        match (os, arch) {
            (zed::Os::Windows, zed::Architecture::X8664) => {
                "laravel-lsp-windows-x64.exe".to_string()
            }
            (zed::Os::Windows, zed::Architecture::Aarch64) => {
                "laravel-lsp-windows-arm64.exe".to_string()
            }
            (zed::Os::Windows, _) => "laravel-lsp.exe".to_string(),
            (zed::Os::Mac, zed::Architecture::Aarch64) => "laravel-lsp-macos-arm64".to_string(),
            (zed::Os::Mac, zed::Architecture::X8664) => "laravel-lsp-macos-x64".to_string(),
            (zed::Os::Mac, _) => "laravel-lsp".to_string(),
            (zed::Os::Linux, zed::Architecture::X8664) => "laravel-lsp-linux-x64".to_string(),
            (zed::Os::Linux, zed::Architecture::Aarch64) => "laravel-lsp-linux-arm64".to_string(),
            (zed::Os::Linux, _) => "laravel-lsp".to_string(),
        }
    }
}

zed::register_extension!(LaravelExtension);
