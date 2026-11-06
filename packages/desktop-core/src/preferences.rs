//! Desktop preferences persisted in `desktop-preferences.json`.
//!
//! The existing schema, including `deny_unknown_fields`, is preserved across upgrades.

use std::collections::BTreeMap;

/// Global actions: id, description, default chord.
pub const ACTIONS: [(&str, &str, &str); 5] = [
    ("start-share", "Start screenshare", "Control+Shift+F7"),
    ("stop-share", "Stop screenshare", "Control+Shift+F8"),
    ("close-room", "Close room", "Control+Shift+F9"),
    (
        "switch-source",
        "Switch screenshare source",
        "Control+Shift+F10",
    ),
    ("toggle-mute", "Toggle audio mute", "Control+Shift+F11"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppearanceMode {
    System,
    Light,
    Dark,
}

impl AppearanceMode {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorTheme {
    Slopcast,
    Ayu,
    RosePine,
    Kanagawa,
    Dracula,
}

impl ColorTheme {
    pub const ALL: [Self; 5] = [
        Self::Slopcast,
        Self::Ayu,
        Self::RosePine,
        Self::Kanagawa,
        Self::Dracula,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Slopcast => "Slopcast",
            Self::Ayu => "Ayu",
            Self::RosePine => "Rosé Pine",
            Self::Kanagawa => "Kanagawa",
            Self::Dracula => "Dracula",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FontFamily {
    #[default]
    Theme,
    System,
    Inter,
    Lora,
    JetbrainsMono,
}

impl FontFamily {
    pub const ALL: [Self; 5] = [
        Self::Theme,
        Self::System,
        Self::Inter,
        Self::Lora,
        Self::JetbrainsMono,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Theme => "Theme default",
            Self::System => "System UI",
            Self::Inter => "Inter",
            Self::Lora => "Lora",
            Self::JetbrainsMono => "JetBrains Mono",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct TypographyPreferences {
    pub font_family: FontFamily,
    pub font_size: Option<u8>,
    pub font_weight: Option<u16>,
    pub ligatures: bool,
}

impl Default for TypographyPreferences {
    fn default() -> Self {
        Self {
            font_family: FontFamily::Theme,
            font_size: None,
            font_weight: None,
            ligatures: true,
        }
    }
}

impl TypographyPreferences {
    /// # Errors
    /// Returns the user-facing reason the typography choice is out of range.
    pub fn validate(&self) -> Result<(), String> {
        if self
            .font_size
            .is_some_and(|size| !(12..=18).contains(&size))
        {
            return Err("Font size must be between 12 and 18 px".into());
        }
        if self
            .font_weight
            .is_some_and(|weight| ![400, 500, 600].contains(&weight))
        {
            return Err("Font weight must be 400, 500 or 600".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct DesktopPreferences {
    pub tray_enabled: bool,
    pub auto_select_audio: bool,
    pub appearance_mode: AppearanceMode,
    pub color_theme: ColorTheme,
    pub typography: TypographyPreferences,
    pub shortcuts: BTreeMap<String, String>,
    pub telemetry_shortcut: String,
}

#[must_use]
pub fn default_telemetry_shortcut() -> String {
    if cfg!(target_os = "macos") {
        "Command+KeyT"
    } else {
        "Control+KeyT"
    }
    .into()
}

#[must_use]
pub fn default_shortcuts() -> BTreeMap<String, String> {
    ACTIONS
        .iter()
        .map(|(action, _, chord)| ((*action).into(), (*chord).into()))
        .collect()
}

impl Default for DesktopPreferences {
    fn default() -> Self {
        Self {
            tray_enabled: true,
            auto_select_audio: true,
            appearance_mode: AppearanceMode::Dark,
            color_theme: ColorTheme::Slopcast,
            typography: TypographyPreferences::default(),
            telemetry_shortcut: default_telemetry_shortcut(),
            shortcuts: default_shortcuts(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_files_round_trip_and_unknown_fields_are_rejected() {
        let json = r#"{"trayEnabled":false,"autoSelectAudio":true,"appearanceMode":"system","colorTheme":"rose-pine","typography":{"fontFamily":"jetbrains-mono","fontSize":14,"fontWeight":500,"ligatures":false},"shortcuts":{"start-share":"Control+Shift+F7"},"telemetryShortcut":""}"#;
        let parsed: DesktopPreferences = serde_json::from_str(json).unwrap_or_default();
        assert!(!parsed.tray_enabled);
        assert_eq!(parsed.color_theme, ColorTheme::RosePine);
        assert_eq!(parsed.typography.font_family, FontFamily::JetbrainsMono);
        assert_eq!(parsed.telemetry_shortcut, "");
        let back = serde_json::to_string(&parsed).unwrap_or_default();
        assert!(back.contains("\"rose-pine\""));
        assert!(serde_json::from_str::<DesktopPreferences>(r#"{"bogus":1}"#).is_err());
        let partial: DesktopPreferences =
            serde_json::from_str(r#"{"trayEnabled":false}"#).unwrap_or_default();
        assert_eq!(partial.shortcuts, default_shortcuts());
    }

    #[test]
    fn typography_is_validated() {
        let mut typography = TypographyPreferences::default();
        assert!(typography.validate().is_ok());
        typography.font_size = Some(20);
        assert!(typography.validate().is_err());
        typography.font_size = Some(14);
        typography.font_weight = Some(700);
        assert!(typography.validate().is_err());
    }
}
