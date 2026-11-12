use std::path::{Path, PathBuf};

/// Remove Windows verbatim prefixes (`\\?\`, `\??\`) so paths look like what users type.
pub fn clean(s: &str) -> String {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return rest.to_string();
    }
    if let Some(rest) = s.strip_prefix(r"\??\") {
        return rest.to_string();
    }
    s.to_string()
}

pub fn display_path(p: &Path) -> String {
    clean(&p.to_string_lossy())
}

pub fn clean_path(p: &Path) -> PathBuf {
    PathBuf::from(display_path(p))
}

/// Normalise user input: `C:` -> `C:\`, trims whitespace and quotes.
pub fn normalize_input(s: &str) -> PathBuf {
    let s = s.trim().trim_matches('"');
    #[cfg(windows)]
    {
        let s = s.replace('/', "\\");
        if s.len() == 2 && s.ends_with(':') {
            return PathBuf::from(format!("{s}\\"));
        }
        PathBuf::from(s)
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(s)
    }
}

pub fn fmt_bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

pub fn file_name_string(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| display_path(p))
}
