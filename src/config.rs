use std::path::PathBuf;

#[derive(Clone, Debug)]
pub enum ExtensionFilter {
    All,
    Include(Vec<String>), // lowercase, no leading dot, e.g. ["jpg", "png"]
    Exclude(Vec<String>),
}

impl ExtensionFilter {
    /// `ext` is the file's extension (lowercase, no leading dot), or `None` if the file has none.
    pub fn matches(&self, ext: Option<&str>) -> bool {
        match self {
            ExtensionFilter::All => true,
            ExtensionFilter::Include(list) => ext.is_some_and(|e| list.iter().any(|x| x == e)),
            ExtensionFilter::Exclude(list) => ext.is_none_or(|e| !list.iter().any(|x| x == e)),
        }
    }
}

/// Which notion of "duplicate" a scan looks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanMode {
    /// Byte-identical content, found via content hashing.
    ExactContent,
    /// Images/videos that look like the same shot at a different resolution
    /// (re-encodes, resizes, thumbnails), found via perceptual hashing.
    SimilarMedia,
    /// Every file passing the size/extension/name filters, duplicate or not,
    /// e.g. to clear out all `.tmp` files or everything under 1 KB.
    MatchingFiles,
}

#[derive(Clone, Debug)]
pub struct ScanConfig {
    pub folders: Vec<PathBuf>,
    pub exclude_subfolders: bool, // true => max_depth(1) per root
    pub same_folder_only: bool,   // true => only mark files duplicate if they share a parent dir
    pub min_size: Option<u64>,    // bytes
    pub max_size: Option<u64>,    // bytes
    pub extensions: ExtensionFilter,
    pub mode: ScanMode,
    /// Max Hamming distance (out of 64 bits) between two perceptual hashes to
    /// still count as the same picture, only used in `ScanMode::SimilarMedia`.
    pub similarity_threshold: u32,
    /// Leave RAR archives and split-archive volumes out of the scan (see
    /// `crate::archive`): deleting one part of a set as "a duplicate" breaks
    /// the whole set.
    pub skip_archives: bool,
    /// Only used in `ScanMode::MatchingFiles`: a case-insensitive name
    /// filter, either a plain substring or a `*`/`?` wildcard pattern
    /// matched against the whole file name. Empty matches everything.
    pub name_filter: String,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            folders: Vec::new(),
            exclude_subfolders: false,
            same_folder_only: false,
            min_size: None,
            max_size: None,
            extensions: ExtensionFilter::All,
            mode: ScanMode::ExactContent,
            similarity_threshold: 10,
            skip_archives: true,
            name_filter: String::new(),
        }
    }
}

/// Case-insensitive file-name match for `ScanConfig::name_filter`: with `*`
/// (any run of characters) or `?` (any one character) the pattern must
/// match the whole name, otherwise it only has to appear somewhere in it.
pub fn name_matches(pattern: &str, name: &str) -> bool {
    let pattern = pattern.trim().to_lowercase();
    if pattern.is_empty() {
        return true;
    }
    let name = name.to_lowercase();
    if !pattern.contains(['*', '?']) {
        return name.contains(&pattern);
    }
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    // Classic greedy wildcard match with backtracking to the last `*`.
    let (mut pi, mut ni) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// Parses a comma-separated extension list into normalized (lowercase, no leading dot) entries.
pub fn parse_extension_list(text: &str) -> Vec<String> {
    text.split(',')
        .map(|s| s.trim().trim_start_matches('.').to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeUnit {
    Bytes,
    KB,
    MB,
    GB,
}

impl SizeUnit {
    pub const ALL: [SizeUnit; 4] = [SizeUnit::Bytes, SizeUnit::KB, SizeUnit::MB, SizeUnit::GB];

    pub fn label(self) -> &'static str {
        match self {
            SizeUnit::Bytes => "B",
            SizeUnit::KB => "KB",
            SizeUnit::MB => "MB",
            SizeUnit::GB => "GB",
        }
    }

    pub fn multiplier(self) -> u64 {
        match self {
            SizeUnit::Bytes => 1,
            SizeUnit::KB => 1024,
            SizeUnit::MB => 1024 * 1024,
            SizeUnit::GB => 1024 * 1024 * 1024,
        }
    }

    /// Parses `text` as a number in this unit and converts it to bytes.
    pub fn parse_to_bytes(self, text: &str) -> Option<u64> {
        if text.trim().is_empty() {
            return None;
        }
        let value: f64 = text.trim().parse().ok()?;
        if value < 0.0 {
            return None;
        }
        Some((value * self.multiplier() as f64) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_filter_is_a_substring_or_a_whole_name_wildcard() {
        assert!(name_matches("", "anything.txt"));
        assert!(name_matches("thumbs", "Thumbs.db"));
        assert!(!name_matches("thumbs", "photo.jpg"));
        assert!(name_matches("*.tmp", "report.TMP"));
        assert!(!name_matches("*.tmp", "report.tmp.bak"));
        assert!(name_matches("img_????.jpg", "IMG_0042.jpg"));
        assert!(!name_matches("img_????.jpg", "IMG_42.jpg"));
        assert!(name_matches("*copy*", "photo - copy (2).png"));
    }
}
