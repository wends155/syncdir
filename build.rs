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
    file.read_exact(&mut header).map_err(|e| BuildResourceError::Io {
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

    pub fn resolve_toolkit_dir_with<F>(&self, _checker: F) -> Result<Option<std::path::PathBuf>, BuildResourceError>
    where
        F: Fn(&std::path::Path) -> bool,
    {
        Ok(None)
    }
}

#[cfg(all(windows, not(test)))]
fn compile_windows_resources() -> Result<(), BuildResourceError> {
    let mut res = winres::WindowsResource::new();
    res.set_icon("syncdir.ico");
    res.compile()
        .map_err(|e| BuildResourceError::CompilationFailed(e.to_string()))
}

#[cfg(any(not(windows), test))]
fn compile_windows_resources() -> Result<(), BuildResourceError> {
    Ok(())
}

#[cfg(not(test))]
fn main() {
    println!("cargo:rerun-if-changed=syncdir.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        if let Err(e) = compile_windows_resources() {
            println!("cargo:warning=Failed to compile Windows resource icon: {e}");
        }
    }
}
