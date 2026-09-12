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

pub fn validate_icon_bytes(_bytes: &[u8]) -> Result<(), BuildResourceError> {
    Err(BuildResourceError::IconInvalid("Stub".into()))
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
