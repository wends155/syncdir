use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
pub enum BuildResourceError {
    IconNotFound(PathBuf),
    IconInvalid(String),
    UnsafePath(String),
    CompilationFailed(String),
    Io {
        path: PathBuf,
        kind: std::io::ErrorKind,
        message: String,
    },
}

impl std::fmt::Display for BuildResourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IconNotFound(path) => {
                write!(f, "Application icon not found at '{}'", path.display())
            }
            Self::IconInvalid(msg) => write!(f, "Invalid application icon: {msg}"),
            Self::UnsafePath(msg) => write!(f, "Unsafe toolkit path rejected: {msg}"),
            Self::CompilationFailed(msg) => write!(f, "Resource compiler failed: {msg}"),
            Self::Io { path, message, .. } => {
                write!(f, "I/O failure accessing '{}': {message}", path.display())
            }
        }
    }
}

impl std::error::Error for BuildResourceError {}

pub fn validate_icon_bytes(bytes: &[u8]) -> Result<(), BuildResourceError> {
    if bytes.len() < 6 {
        return Err(BuildResourceError::IconInvalid(format!(
            "Icon header too short: expected 6 bytes, got {}",
            bytes.len()
        )));
    }
    let reserved = u16::from_le_bytes([bytes[0], bytes[1]]);
    if reserved != 0 {
        return Err(BuildResourceError::IconInvalid(format!(
            "Invalid ICO reserved word: expected 0, got {reserved}"
        )));
    }
    let res_type = u16::from_le_bytes([bytes[2], bytes[3]]);
    if res_type != 1 {
        return Err(BuildResourceError::IconInvalid(format!(
            "Invalid ICO resource type: expected 1, got {res_type}"
        )));
    }
    let image_count = u16::from_le_bytes([bytes[4], bytes[5]]);
    if image_count == 0 {
        return Err(BuildResourceError::IconInvalid(
            "Invalid ICO file: image directory contains zero images".into(),
        ));
    }
    Ok(())
}

pub fn validate_icon_asset(path: &std::path::Path) -> Result<(), BuildResourceError> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => BuildResourceError::IconNotFound(path.to_path_buf()),
        kind => BuildResourceError::Io {
            path: path.to_path_buf(),
            kind,
            message: e.to_string(),
        },
    })?;

    let meta = file.metadata().map_err(|e| BuildResourceError::Io {
        path: path.to_path_buf(),
        kind: e.kind(),
        message: e.to_string(),
    })?;

    if !meta.is_file() {
        return Err(BuildResourceError::IconInvalid(format!(
            "Path '{}' is not a regular file",
            path.display()
        )));
    }

    let len = meta.len();
    if len == 0 {
        return Err(BuildResourceError::IconInvalid("Icon file is empty".into()));
    }
    if len > 524_288 {
        return Err(BuildResourceError::IconInvalid(format!(
            "Icon file size ({len} bytes) exceeds maximum limit of 512 KB"
        )));
    }

    let mut header = [0u8; 6];
    file.read_exact(&mut header)
        .map_err(|e| BuildResourceError::Io {
            path: path.to_path_buf(),
            kind: e.kind(),
            message: e.to_string(),
        })?;

    validate_icon_bytes(&header)
}

pub fn validate_path_safety(path: &std::path::Path) -> Result<(), BuildResourceError> {
    let path_str = path.to_str().ok_or_else(|| {
        BuildResourceError::UnsafePath("Path contains invalid UTF-8 characters".to_string())
    })?;

    if path_str.contains('\0') {
        return Err(BuildResourceError::UnsafePath(
            "Path contains prohibited null byte".to_string(),
        ));
    }
    if path_str.contains('"') {
        return Err(BuildResourceError::UnsafePath(
            "Path contains prohibited quote characters".to_string(),
        ));
    }
    if path_str.chars().any(|c| c.is_control()) {
        return Err(BuildResourceError::UnsafePath(
            "Path contains prohibited control characters".to_string(),
        ));
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceBuildConfig {
    pub winres_toolkit_path: Option<std::path::PathBuf>,
    pub rc_path: Option<std::path::PathBuf>,
    pub windows_sdk_path: Option<std::path::PathBuf>,
    pub allow_missing_icon: bool,
    pub profile: String,
    pub target_os: String,
}

impl ResourceBuildConfig {
    pub fn from_env() -> Self {
        Self::from_env_with(|var| std::env::var(var).ok())
    }

    pub fn from_env_with<E>(lookup: E) -> Self
    where
        E: Fn(&str) -> Option<String>,
    {
        let read_path = |var: &str| {
            lookup(var)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .map(std::path::PathBuf::from)
        };
        let allow_missing = lookup("SYNCDIR_ALLOW_MISSING_ICON")
            .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);

        Self {
            winres_toolkit_path: read_path("WINRES_TOOLKIT_PATH"),
            rc_path: read_path("RC_PATH"),
            windows_sdk_path: read_path("WINDOWS_SDK_PATH"),
            allow_missing_icon: allow_missing,
            profile: lookup("PROFILE").unwrap_or_default(),
            target_os: lookup("CARGO_CFG_TARGET_OS").unwrap_or_default(),
        }
    }

    #[must_use]
    pub fn is_release(&self) -> bool {
        self.profile == "release"
    }

    #[must_use]
    pub fn allow_missing_icon(&self) -> bool {
        self.allow_missing_icon
    }

    pub fn resolve_toolkit_dir(&self) -> Result<Option<std::path::PathBuf>, BuildResourceError> {
        self.resolve_toolkit_dir_with(|p| p.exists())
    }

    pub fn resolve_toolkit_dir_with<F>(
        &self,
        checker: F,
    ) -> Result<Option<std::path::PathBuf>, BuildResourceError>
    where
        F: Fn(&std::path::Path) -> bool,
    {
        // Tier 1: Explicit toolkit directory override
        if let Some(ref path) = self.winres_toolkit_path {
            validate_path_safety(path)?;
            if checker(path) {
                return Ok(Some(path.clone()));
            }
            println!(
                "cargo:warning=WINRES_TOOLKIT_PATH ('{}') does not exist or is invalid; falling back.",
                path.display()
            );
        }

        // Tier 2: Explicit resource compiler executable or enclosing directory
        if let Some(ref path) = self.rc_path {
            validate_path_safety(path)?;
            if let Some(resolved) = Self::resolve_rc_path_with(path, &checker) {
                return Ok(Some(resolved));
            }
            println!(
                "cargo:warning=RC_PATH ('{}') does not exist or is invalid; falling back.",
                path.display()
            );
        }

        // Tier 3: Windows SDK root probing
        if let Some(ref root) = self.windows_sdk_path {
            validate_path_safety(root)?;
            if let Some(bin_dir) = Self::probe_sdk_bin_with(root, &checker) {
                return Ok(Some(bin_dir));
            }
            if checker(root) {
                return Ok(Some(root.clone()));
            }
            println!(
                "cargo:warning=WINDOWS_SDK_PATH ('{}') is invalid or contains no resource compiler; falling back.",
                root.display()
            );
        }

        // Tier 4: Fall back to winres built-in registry / PATH discovery
        Ok(None)
    }

    fn resolve_rc_path_with<F>(path: &std::path::Path, checker: &F) -> Option<std::path::PathBuf>
    where
        F: Fn(&std::path::Path) -> bool,
    {
        // If the path itself is a directory containing rc.exe or windres.exe:
        if checker(&path.join("rc.exe")) || checker(&path.join("windres.exe")) {
            return Some(path.to_path_buf());
        }
        // If the path points directly to rc.exe / windres.exe binary:
        if checker(path) {
            let is_binary = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|name| {
                    let lower = name.to_ascii_lowercase();
                    lower == "rc.exe" || lower == "windres.exe"
                })
                .unwrap_or(false);

            if is_binary {
                return path.parent().map(std::path::Path::to_path_buf);
            }
        }
        None
    }

    fn probe_sdk_bin_with<F>(sdk_path: &std::path::Path, checker: &F) -> Option<std::path::PathBuf>
    where
        F: Fn(&std::path::Path) -> bool,
    {
        let candidates = [
            sdk_path.join("bin").join("x64").join("rc.exe"),
            sdk_path.join("bin").join("x86").join("rc.exe"),
            sdk_path.join("bin").join("rc.exe"),
            sdk_path.join("rc.exe"),
        ];

        for candidate in &candidates {
            if checker(candidate) {
                return candidate.parent().map(std::path::Path::to_path_buf);
            }
        }

        None
    }
}

pub fn emit_rebuild_directives() {
    println!("cargo:rerun-if-changed=syncdir.ico");
    println!("cargo:rerun-if-env-changed=WINRES_TOOLKIT_PATH");
    println!("cargo:rerun-if-env-changed=RC_PATH");
    println!("cargo:rerun-if-env-changed=WINDOWS_SDK_PATH");
    println!("cargo:rerun-if-env-changed=SYNCDIR_ALLOW_MISSING_ICON");
}

pub fn handle_resource_error(err: &BuildResourceError, config: &ResourceBuildConfig) {
    if config.is_release() && !config.allow_missing_icon() {
        eprintln!(
            "================================================================================"
        );
        eprintln!("ERROR: Failed to compile Windows resource icon for release build: {err}");
        eprintln!(
            "================================================================================"
        );
        eprintln!("Release builds must embed the application icon and manifest to ensure identity");
        eprintln!("and prevent legacy Windows UAC virtualization (CWE-390 / CWE-250).");
        eprintln!();
        eprintln!("Remediation options:");
        eprintln!("  1. Set WINRES_TOOLKIT_PATH to directory containing rc.exe or windres.exe:");
        eprintln!("     $env:WINRES_TOOLKIT_PATH = 'C:\\path\\to\\bin\\x64'");
        eprintln!("  2. Set RC_PATH directly to the resource compiler binary:");
        eprintln!("     $env:RC_PATH = 'C:\\path\\to\\bin\\x64\\rc.exe'");
        eprintln!("  3. Set WINDOWS_SDK_PATH to the Windows SDK root folder:");
        eprintln!("     $env:WINDOWS_SDK_PATH = 'C:\\Program Files (x86)\\Windows Kits\\10'");
        eprintln!("  4. Ensure Windows 10/11 SDK or Build Tools with rc.exe is installed.");
        eprintln!("  5. Headless / CI escape hatch (suppresses error and continues without icon):");
        eprintln!("     $env:SYNCDIR_ALLOW_MISSING_ICON = '1'");
        eprintln!(
            "================================================================================"
        );
        std::process::exit(1);
    } else {
        println!("cargo:warning=Failed to compile Windows resource icon: {err}");
        println!(
            "cargo:warning=Remediation: Set WINRES_TOOLKIT_PATH, RC_PATH, or WINDOWS_SDK_PATH, or set SYNCDIR_ALLOW_MISSING_ICON=1"
        );
    }
}

#[cfg(all(windows, not(test)))]
pub fn compile_windows_resources(config: &ResourceBuildConfig) -> Result<(), BuildResourceError> {
    let icon_path = std::path::Path::new("syncdir.ico");
    validate_icon_asset(icon_path)?;

    let mut res = winres::WindowsResource::new();
    res.set_icon("syncdir.ico");

    if let Some(toolkit) = config.resolve_toolkit_dir()? {
        res.set_toolkit_path(&toolkit.to_string_lossy());
    }

    res.compile()
        .map_err(|e| BuildResourceError::CompilationFailed(e.to_string()))
}

#[cfg(any(not(windows), test))]
pub fn compile_windows_resources(_config: &ResourceBuildConfig) -> Result<(), BuildResourceError> {
    Ok(())
}

#[cfg(not(test))]
fn main() {
    emit_rebuild_directives();

    let config = ResourceBuildConfig::from_env();
    let is_windows =
        config.target_os == "windows" || (config.target_os.is_empty() && cfg!(windows));
    if is_windows {
        match compile_windows_resources(&config) {
            Ok(()) => {}
            Err(e) => handle_resource_error(&e, &config),
        }
    }
}
