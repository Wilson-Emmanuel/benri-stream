use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
}
