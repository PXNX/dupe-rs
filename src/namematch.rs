/// Strips `marker` from `s` if it appears followed only by whitespace and/or
/// digits until the end (e.g. marker `" copy"` matches both `"x copy"` and
/// `"x copy 2"`, but not `"x copy machine"`).
fn strip_trailing_marker(s: &str, marker: &str) -> Option<String> {
    let idx = s.rfind(marker)?;
    let tail = s[idx + marker.len()..].trim();
    if tail.is_empty() || tail.chars().all(|c| c.is_ascii_digit()) {
        Some(s[..idx].to_string())
    } else {
        None
    }
}

/// Repeatedly strips well-known "this is a copy" filename decorations —
/// Windows' `" (2)"`, German Explorer's `" - Kopie"` / `" - Kopie (2)"`,
/// macOS' `" copy"` / `" copy 2"`, and `"Copy of "` / `"Kopie von "` prefixes —
/// until none apply, leaving the presumed base name.
fn strip_copy_decorations(stem: &str) -> String {
    let mut s = stem.trim().to_string();
    loop {
        let before = s.clone();

        for prefix in ["Copy of ", "copy of ", "Kopie von ", "kopie von "] {
            if let Some(rest) = s.strip_prefix(prefix) {
                s = rest.trim().to_string();
            }
        }

        // Trailing "(N)"-style counters, e.g. "photo (2)" -> "photo".
        if s.ends_with(')')
            && let Some(open) = s.rfind('(')
        {
            let inside = &s[open + 1..s.len() - 1];
            if !inside.is_empty() && inside.chars().all(|c| c.is_ascii_digit()) {
                s = s[..open].trim_end().to_string();
            }
        }

        for marker in [" - Kopie", " - kopie", " Kopie", " copy", " Copy", "_copy", "_Kopie"] {
            if let Some(stripped) = strip_trailing_marker(&s, marker) {
                s = stripped;
                break;
            }
        }

        if s == before {
            break;
        }
    }
    s.trim().to_string()
}

/// True if `candidate`'s filename looks like an OS/user-generated copy of
/// `original` — same base name once known "copy" decorations are stripped,
/// but not literally identical (that'd just be the same name, not a copy of it).
pub fn looks_like_copy(original_stem: &str, candidate_stem: &str) -> bool {
    if original_stem.eq_ignore_ascii_case(candidate_stem) {
        return false;
    }
    let normalized_candidate = strip_copy_decorations(candidate_stem);
    let normalized_original = strip_copy_decorations(original_stem);
    !normalized_candidate.is_empty() && normalized_candidate.eq_ignore_ascii_case(&normalized_original)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_windows_style_counter_suffix() {
        assert!(looks_like_copy("photo", "photo (2)"));
        assert!(looks_like_copy("photo", "photo (17)"));
    }

    #[test]
    fn detects_german_kopie_suffix_with_and_without_counter() {
        assert!(looks_like_copy("Rechnung", "Rechnung - Kopie"));
        assert!(looks_like_copy("Rechnung", "Rechnung - Kopie (2)"));
    }

    #[test]
    fn detects_macos_style_copy_suffix() {
        assert!(looks_like_copy("report", "report copy"));
        assert!(looks_like_copy("report", "report copy 2"));
    }

    #[test]
    fn detects_copy_of_prefix() {
        assert!(looks_like_copy("Budget", "Copy of Budget"));
        assert!(looks_like_copy("Budget", "Kopie von Budget"));
    }

    #[test]
    fn identical_names_are_not_a_copy_of_each_other() {
        assert!(!looks_like_copy("photo", "photo"));
        assert!(!looks_like_copy("Photo", "photo"));
    }

    #[test]
    fn unrelated_names_are_not_flagged() {
        assert!(!looks_like_copy("report_final", "report_draft"));
        assert!(!looks_like_copy("a", "b"));
    }
}
