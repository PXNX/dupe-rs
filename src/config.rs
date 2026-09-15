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

#[derive(Clone, Debug)]
pub struct ScanConfig {
    pub folders: Vec<PathBuf>,
    pub exclude_subfolders: bool, // true => max_depth(1) per root
    pub min_size: Option<u64>,    // bytes
    pub max_size: Option<u64>,    // bytes
    pub extensions: ExtensionFilter,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            folders: Vec::new(),
            exclude_subfolders: false,
            min_size: None,
            max_size: None,
            extensions: ExtensionFilter::All,
        }
    }
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
