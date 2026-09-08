use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Codex,
    Gemini,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
            Provider::Gemini => "gemini",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    NotAuthenticated,
    Expired,
    NetworkError,
    RateLimited,
    UnknownError,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageWindow {
    pub key: String,
    pub name: String,
    pub utilization: f64,
    #[serde(rename = "resetsAt")]
    pub resets_at: String,
    #[serde(rename = "timeProgress")]
    pub time_progress: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtraUsage {
    #[serde(rename = "isEnabled")]
    pub is_enabled: bool,
    #[serde(rename = "monthlyLimit")]
    pub monthly_limit: f64,
    #[serde(rename = "usedCredits")]
    pub used_credits: f64,
    pub utilization: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageResponse {
    pub provider: Provider,
    pub status: Status,
    pub windows: Vec<UsageWindow>,
    #[serde(rename = "extraUsage", skip_serializing_if = "Option::is_none")]
    pub extra_usage: Option<ExtraUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ViewMode {
    Normal,
    Mini,
    Super,
}

impl Default for ViewMode {
    fn default() -> Self { ViewMode::Normal }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub window: WindowRect,
    #[serde(rename = "alwaysOnTop")]
    pub always_on_top: bool,
    pub opacity: f64,
    #[serde(rename = "refreshIntervalSec")]
    pub refresh_interval_sec: u64,
    pub autostart: bool,
    #[serde(rename = "viewMode", default)]
    pub view_mode: ViewMode,
    #[serde(rename = "closeToTray", default)]
    pub close_to_tray: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            window: WindowRect { x: 1580, y: 40, width: 320, height: 520 },
            always_on_top: true,
            opacity: 0.92,
            refresh_interval_sec: 300,
            autostart: false,
            view_mode: ViewMode::Normal,
            close_to_tray: false,
        }
    }
}
