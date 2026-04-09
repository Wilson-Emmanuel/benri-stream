use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum QualityLevel {
    Low,
    Medium,
    High,
}

impl QualityLevel {
    pub fn resolution(&self) -> (u32, u32) {
        match self {
            Self::Low => (640, 360),
            Self::Medium => (1280, 720),
            Self::High => (1920, 1080),
        }
    }

    pub fn target_bitrate_bps(&self) -> u32 {
        match self {
            Self::Low => 800_000,
            Self::Medium => 2_500_000,
            Self::High => 5_000_000,
        }
    }

    pub fn all() -> &'static [QualityLevel] {
        &[Self::Low, Self::Medium, Self::High]
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    /// Parse a single tier name (case-insensitive). Returns `None` for
    /// anything unrecognised so the caller can log a warning and fall
    /// back to the default ladder rather than crash at startup on a
    /// typo.
    pub fn from_name(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }
}

/// Parse a comma-separated tier list (e.g. `"low,medium,high"` or
/// `"low"`) into an ordered, de-duplicated `Vec<QualityLevel>`. Unknown
/// entries are dropped and logged. An empty or all-unknown list
/// resolves to the default full ladder so a misconfigured env var
/// degrades gracefully rather than killing the worker.
///
/// Order of the input is preserved so the master playlist lists
/// variants in the same order the operator specified; the viewer's
/// player picks the first variant by default, so putting the cheapest
/// tier first is usually what you want.
pub fn parse_quality_tiers(raw: &str) -> Vec<QualityLevel> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for piece in raw.split(',') {
        match QualityLevel::from_name(piece) {
            Some(level) if seen.insert(level) => out.push(level),
            Some(_) => {
                // Duplicate — silently ignore.
            }
            None if !piece.trim().is_empty() => {
                tracing::warn!(value = %piece, "unknown quality tier, ignoring");
            }
            None => {}
        }
    }
    if out.is_empty() {
        tracing::warn!("quality tier list empty or all unknown; falling back to default ladder");
        return QualityLevel::all().to_vec();
    }
    out
}

