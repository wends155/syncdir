//! Configuration validation rules, limits, and TOML preprocessing helpers.

/// Maximum supported block size for chunked synchronization (64MB).
pub(crate) const MAX_BLOCK_SIZE_BYTES: u64 = 64 * 1024 * 1024;

/// Default debounce interval in seconds.
pub(crate) const DEFAULT_DEBOUNCE_SECONDS: u64 = 3;

/// Default retry interval in seconds.
pub(crate) const DEFAULT_RETRY_INTERVAL_SECONDS: u64 = 10;

/// Preprocess raw TOML configuration text to escape single backslashes in Windows file paths.
///
/// Converts Windows paths like `"C:\Users\path"` to `"C:\\Users\\path"` while preserving
/// UNC prefixes and existing escapes so standard TOML parsers succeed.
#[must_use]
pub(crate) fn preprocess_config_toml(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut in_dest_dirs_array = false;

    let has_bracket_outside_quotes = |s: &str, target: char| -> bool {
        let mut in_q = false;
        for ch in s.chars() {
            if ch == '"' {
                in_q = !in_q;
            } else if ch == target && !in_q {
                return true;
            }
        }
        false
    };

    for line in content.lines() {
        let trimmed = line.trim();
        let is_config_line = (trimmed.starts_with("source_dir") || trimmed.starts_with("dest_dir"))
            && trimmed.contains('=');

        let starts_dest_dirs = trimmed.starts_with("dest_dirs") && trimmed.contains('=');

        if starts_dest_dirs {
            // Check if array is multi-line (has opening bracket but no closing bracket outside quotes on this line)
            if has_bracket_outside_quotes(trimmed, '[') && !has_bracket_outside_quotes(trimmed, ']')
            {
                in_dest_dirs_array = true;
            }
        }

        if is_config_line || starts_dest_dirs || in_dest_dirs_array {
            let processed = escape_backslashes_in_quotes(line);
            result.push_str(&processed);
            result.push('\n');

            if in_dest_dirs_array && has_bracket_outside_quotes(trimmed, ']') {
                in_dest_dirs_array = false;
            }
            continue;
        }

        result.push_str(line);
        result.push('\n');
    }
    result
}

/// Escape unescaped backslashes inside double-quoted string literals.
#[must_use]
pub(crate) fn escape_backslashes_in_quotes(line: &str) -> String {
    let mut result = String::with_capacity(line.len() * 2);
    let mut in_quotes = false;
    let mut is_start_of_quote = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            in_quotes = !in_quotes;
            is_start_of_quote = in_quotes;
            result.push('"');
        } else if c == '\\' && in_quotes {
            let mut count = 1;
            while chars.peek() == Some(&'\\') {
                count += 1;
                chars.next();
            }
            if is_start_of_quote && count == 2 {
                result.push_str(r"\\\\");
            } else if count == 1 {
                result.push_str(r"\\");
            } else {
                for _ in 0..count {
                    result.push('\\');
                }
            }
            is_start_of_quote = false;
        } else {
            if in_quotes {
                is_start_of_quote = false;
            }
            result.push(c);
        }
    }
    result
}

use std::path::Path;

use crate::error::SyncError;

/// Validates that neither `source` nor `dest` is identical to or nested within the other,
/// preventing recursive synchronization loops.
pub(crate) fn validate_sync_boundaries(source: &Path, dest: &Path) -> Result<(), SyncError> {
    if crate::path_util::is_same_or_descendant(source, dest)
        || crate::path_util::is_same_or_descendant(dest, source)
    {
        return Err(SyncError::validation_loop(format!(
            "Destination directory '{}' is identical to or nested within source directory '{}' (recursive sync loop)",
            dest.display(),
            source.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_validate_sync_boundaries_identical_and_nested() {
        let src = Path::new(r"C:\data\sync");
        let same = Path::new(r"C:\data\sync");
        let child = Path::new(r"C:\data\sync\nested\dest");
        let parent = Path::new(r"C:\data");
        let separate = Path::new(r"C:\backup\dest");

        // Separate paths must succeed
        assert!(validate_sync_boundaries(src, separate).is_ok());

        // Identical paths must fail with RecursiveLoop error
        let err_identical = validate_sync_boundaries(src, same).unwrap_err();
        assert!(matches!(
            err_identical,
            SyncError::Validation {
                kind: crate::error::ValidationKind::RecursiveLoop,
                ..
            }
        ));

        // Destination inside source must fail with RecursiveLoop error
        let err_child = validate_sync_boundaries(src, child).unwrap_err();
        assert!(matches!(
            err_child,
            SyncError::Validation {
                kind: crate::error::ValidationKind::RecursiveLoop,
                ..
            }
        ));

        // Source inside destination must fail with RecursiveLoop error
        let err_parent = validate_sync_boundaries(src, parent).unwrap_err();
        assert!(matches!(
            err_parent,
            SyncError::Validation {
                kind: crate::error::ValidationKind::RecursiveLoop,
                ..
            }
        ));
    }
}
