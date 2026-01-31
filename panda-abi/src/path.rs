//! Path canonicalization utilities.
//!
//! Provides path normalization to prevent directory traversal attacks
//! in the VFS layer.

/// Check whether a path is already in canonical form.
///
/// A canonical path:
/// - Starts with `/`
/// - Has no empty components (no `//`)
/// - Has no `.` components
/// - Has no `..` components
/// - Does not end with `/` (unless it is exactly `/`)
pub fn is_canonical(path: &str) -> bool {
    let bytes = path.as_bytes();

    // Must start with /
    if bytes.first() != Some(&b'/') {
        return false;
    }

    // Root path is canonical
    if bytes.len() == 1 {
        return true;
    }

    // Must not end with /
    if bytes.last() == Some(&b'/') {
        return false;
    }

    // Check each component
    let mut i = 1; // skip leading /
    while i < bytes.len() {
        // Find end of component
        let start = i;
        while i < bytes.len() && bytes[i] != b'/' {
            i += 1;
        }
        let component = &bytes[start..i];

        // Empty component means //
        if component.is_empty() {
            return false;
        }

        // Check for . and ..
        if component == b"." || component == b".." {
            return false;
        }

        // Skip the /
        i += 1;
    }

    true
}

/// Write a canonicalized path into the provided buffer.
///
/// Returns the number of bytes written, or `None` if the buffer is too small.
///
/// This function is `no_std` compatible and does not allocate.
pub fn canonicalize_path_to_buf<'a>(path: &str, buf: &'a mut [u8]) -> Option<&'a str> {
    // Collect canonical components
    // We'll track component start/end positions to avoid needing a separate vec
    let mut positions: [(usize, usize); 64] = [(0, 0); 64]; // max 64 components
    let mut count = 0usize;

    for component in path.split('/').filter(|s| !s.is_empty()) {
        match component {
            "." => {}
            ".." => {
                if count > 0 {
                    count -= 1;
                }
            }
            _ => {
                if count >= 64 {
                    return None; // Path too deep
                }
                let start = component.as_ptr() as usize - path.as_ptr() as usize;
                positions[count] = (start, component.len());
                count += 1;
            }
        }
    }

    // Build the result
    if count == 0 {
        if buf.is_empty() {
            return None;
        }
        buf[0] = b'/';
        return Some(unsafe { core::str::from_utf8_unchecked(&buf[..1]) });
    }

    // Calculate needed size
    let mut needed = 0usize;
    for i in 0..count {
        needed += 1 + positions[i].1; // "/" + component
    }

    if needed > buf.len() {
        return None;
    }

    let mut pos = 0;
    for i in 0..count {
        let (start, len) = positions[i];
        buf[pos] = b'/';
        pos += 1;
        buf[pos..pos + len].copy_from_slice(&path.as_bytes()[start..start + len]);
        pos += len;
    }

    Some(unsafe { core::str::from_utf8_unchecked(&buf[..pos]) })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: canonicalize using the buffer-based function and return a String
    fn canon(path: &str) -> std::string::String {
        let mut buf = [0u8; 1024];
        canonicalize_path_to_buf(path, &mut buf)
            .expect("canonicalize_path_to_buf failed")
            .to_string()
    }

    #[test]
    fn root_path() {
        assert_eq!(canon("/"), "/");
    }

    #[test]
    fn simple_path() {
        assert_eq!(canon("/initrd/hello.txt"), "/initrd/hello.txt");
    }

    #[test]
    fn dot_components_removed() {
        assert_eq!(canon("/initrd/./hello.txt"), "/initrd/hello.txt");
    }

    #[test]
    fn dotdot_resolves_parent() {
        assert_eq!(canon("/initrd/subdir/../hello.txt"), "/initrd/hello.txt");
    }

    #[test]
    fn dotdot_at_root_clamped() {
        assert_eq!(canon("/../../../etc"), "/etc");
    }

    #[test]
    fn mount_escape_resolved_before_matching() {
        // Core attack vector: resolves to /disk/secret, NOT /initrd with ../disk/secret
        assert_eq!(canon("/initrd/../disk/secret"), "/disk/secret");
    }

    #[test]
    fn repeated_slashes_collapsed() {
        assert_eq!(canon("///initrd//hello.txt"), "/initrd/hello.txt");
    }

    #[test]
    fn trailing_slash_removed() {
        assert_eq!(canon("/initrd/"), "/initrd");
    }

    #[test]
    fn dotdot_within_mount_stays_in_mount() {
        assert_eq!(canon("/mnt/a/b/../c"), "/mnt/a/c");
    }

    #[test]
    fn empty_after_dotdot_gives_root() {
        assert_eq!(canon("/foo/.."), "/");
    }

    #[test]
    fn complex_mixed_components() {
        assert_eq!(canon("/a/./b/../c/./d/../e"), "/a/c/e");
    }

    #[test]
    fn just_slashes() {
        assert_eq!(canon("///"), "/");
    }

    #[test]
    fn dot_only() {
        assert_eq!(canon("/."), "/");
    }

    #[test]
    fn dotdot_only() {
        assert_eq!(canon("/.."), "/");
    }

    #[test]
    fn multiple_dotdot_from_deep() {
        assert_eq!(canon("/a/b/c/../../d"), "/a/d");
    }

    #[test]
    fn is_canonical_positive() {
        assert!(is_canonical("/"));
        assert!(is_canonical("/foo"));
        assert!(is_canonical("/foo/bar"));
        assert!(is_canonical("/foo/bar/baz.txt"));
    }

    #[test]
    fn is_canonical_negative() {
        assert!(!is_canonical(""));
        assert!(!is_canonical("foo"));
        assert!(!is_canonical("/foo/"));
        assert!(!is_canonical("/foo//bar"));
        assert!(!is_canonical("/foo/./bar"));
        assert!(!is_canonical("/foo/../bar"));
        assert!(!is_canonical("/."));
        assert!(!is_canonical("/.."));
    }

    #[test]
    fn buffer_too_small() {
        let mut buf = [0u8; 3];
        assert!(canonicalize_path_to_buf("/initrd/hello.txt", &mut buf).is_none());
    }

    #[test]
    fn empty_path_gives_root() {
        assert_eq!(canon(""), "/");
    }
}
