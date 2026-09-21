// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Design note: Config sub-types should generally not implement Default unless
// delegating to Config::default(), which parses glide.default.toml. A manual
// Default impl with hardcoded values can silently diverge from the TOML file,
// causing different behavior in code paths that don't load the config file
// (tests, first run, deserialization of saved state).

#[macro_use]
mod partial;
use std::fs::File;
use std::io::Read;
use std::ops::{Deref, Range};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use livesplit_hotkey::Hotkey;
use macro_rules_attribute::derive;
use partial::{PartialConfig, ValidationError};
use regex::{Regex, RegexBuilder};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::actor::wm_controller::WmCommand;
use crate::model::LayoutKind;
use crate::model::scratchpad::FractionalRect;

pub fn data_dir() -> PathBuf {
    dirs::home_dir().unwrap().join(".glide")
}

pub fn restore_file() -> PathBuf {
    data_dir().join("layout.ron")
}

pub fn config_path() -> PathBuf {
    let try_paths = default_config_paths();
    for path in &try_paths {
        if path.try_exists().unwrap_or(false) {
            return path.clone();
        }
    }
    try_paths[0].clone()
}

fn default_config_paths() -> Vec<PathBuf> {
    let home = dirs::home_dir().expect("Could not determine home directory");
    let xdg_path = home.join(".config/glide/glide.toml");
    let legacy_path = home.join(".glide.toml");

    let mut paths = vec![xdg_path.clone()];
    if legacy_path != xdg_path {
        paths.push(legacy_path);
    }
    paths
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Config {
    pub settings: Settings,
    pub window_rules: Vec<WindowRule>,
    pub keys: Vec<(Hotkey, WmCommand)>,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
#[serde(default)]
struct ConfigPartial {
    settings: SettingsPartial,
    window_rules: Option<Vec<WindowRule>>,
    keys: Option<FxHashMap<String, WmCommandOrDisable>>,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum WmCommandOrDisable {
    WmCommand(WmCommand),
    Disable(Disabled),
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Disabled {
    Disable,
}

#[derive(PartialConfig!)]
#[derive_args(SettingsPartial)]
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub animate: bool,
    pub default_disable: bool,
    pub mouse_follows_focus: bool,
    pub mouse_hides_on_focus: bool,
    pub focus_follows_mouse: bool,
    pub outer_gap: f64,
    pub inner_gap: f64,
    pub default_keys: bool,
    pub default_layout_kind: LayoutKind,
    #[derive_args(GroupBarsPartial)]
    pub group_bars: GroupBars,
    #[derive_args(StatusIconPartial)]
    pub status_icon: StatusIcon,
    #[derive_args(ExperimentalPartial)]
    pub experimental: Experimental,
}

/// A [`Regex`] sourced from config. Deserializing compiles (and thus
/// validates) the pattern, so an invalid regex surfaces as a config error at
/// parse time rather than being silently ignored later. Serializes back to the
/// original pattern string, and compares by pattern.
#[derive(Debug, Clone)]
pub struct ConfigRegex(Regex);

impl Deref for ConfigRegex {
    type Target = Regex;
    fn deref(&self) -> &Regex {
        &self.0
    }
}

impl FromStr for ConfigRegex {
    type Err = regex::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        RegexBuilder::new(s).case_insensitive(true).build().map(ConfigRegex)
    }
}

impl PartialEq for ConfigRegex {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_str() == other.0.as_str()
    }
}

impl<'de> Deserialize<'de> for ConfigRegex {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let pattern = String::deserialize(deserializer)?;
        pattern.parse().map_err(serde::de::Error::custom)
    }
}

impl Serialize for ConfigRegex {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.as_str())
    }
}

/// A rule that overrides how a window is managed when it is first observed,
/// based on properties of the window and its application.
///
/// Conditions are nested under `if`; all specified conditions must match
/// (logical AND) for the rule to apply. Rules are evaluated in order and the
/// first matching rule wins. See `window_rules` in the configuration.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(deny_unknown_fields)]
pub struct WindowRule {
    /// Conditions a window must satisfy for this rule to apply.
    #[serde(rename = "if", default)]
    pub conditions: WindowRuleConditions,
    /// Whether matching windows should float (`true`) or tile (`false`).
    #[serde(default)]
    pub float: Option<bool>,
    /// Name of the scratchpad that matching windows belong to. Scratchpad
    /// windows always float.
    #[serde(default)]
    pub scratchpad: Option<String>,
    /// Frame of the scratchpad window as fractions of the active screen.
    #[serde(default)]
    pub frame: Option<FractionalRect>,
}

impl WindowRule {
    pub(crate) fn validated(mut self) -> Result<Self, String> {
        match (&self.scratchpad, self.float) {
            (None, None) => return Err("window rule needs `float` or `scratchpad`".into()),
            (Some(name), _) if name.trim().is_empty() => {
                return Err("window rule `scratchpad` must not be empty".into());
            }
            (Some(_), Some(false)) => {
                return Err("window rule with `scratchpad` cannot set `float = false`".into());
            }
            (None, _) if self.frame.is_some() => {
                return Err("window rule `frame` requires `scratchpad`".into());
            }
            _ => {}
        }
        self.frame = self.frame.map(FractionalRect::validated);
        Ok(self)
    }
}

/// Conditions matched against a window and its application. All specified
/// conditions must match (logical AND); omitted conditions are ignored, so an
/// empty set of conditions matches every window. All conditions match
/// case-insensitively.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Default)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct WindowRuleConditions {
    /// Application bundle identifier, matched exactly (e.g. "com.apple.Safari").
    pub app_id: Option<String>,
    /// Substring match on the application's localized name.
    pub app_name: Option<String>,
    /// Regex matched against the window title. Must be a valid regex or the
    /// config is rejected.
    pub title_regex: Option<ConfigRegex>,
    /// Literal substring match on the window title.
    pub title_substring: Option<String>,
    /// Match for the macOS Accessibility AXRole (e.g. "AXWindow").
    pub ax_role: Option<String>,
    /// Match for the macOS Accessibility AXSubrole (e.g. "AXDialog").
    pub ax_subrole: Option<String>,
}

#[derive(PartialConfig!)]
#[derive_args(ExperimentalPartial)]
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Experimental {
    #[derive_args(StatusIconExperimentalPartial)]
    pub status_icon: StatusIconExperimental,
    #[derive_args(ScrollConfigPartial)]
    pub scroll: ScrollConfig,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum NewWindowPlacement {
    NewColumn,
    SameColumn,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
pub enum CenterMode {
    #[default]
    Never,
    Always,
    OnOverflow,
}

#[derive(PartialConfig!)]
#[derive_args(ScrollConfigPartial)]
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(deny_unknown_fields)]
pub struct ScrollConfig {
    pub enable: bool,
    pub center_focused_column: CenterMode,
    pub visible_columns: u32,
    pub column_width_presets: Vec<f64>,
    pub new_window_in_column: NewWindowPlacement,
    pub scroll_sensitivity: f64,
    pub invert_scroll_direction: bool,
    pub infinite_loop: bool,
    pub single_column_aspect_ratio: String,
}

impl Default for ScrollConfig {
    fn default() -> Self {
        Config::default().settings.experimental.scroll
    }
}

impl ScrollConfig {
    pub fn validated(mut self) -> Self {
        self.visible_columns = self.visible_columns.clamp(1, 5);
        self.scroll_sensitivity = self.scroll_sensitivity.clamp(0.0, 100.0);
        self.column_width_presets.retain(|&p| p > 0.0 && p <= 1.0);
        self
    }

    pub fn aspect_ratio(&self) -> Option<AspectRatio> {
        if self.single_column_aspect_ratio.is_empty() {
            return None;
        }
        AspectRatio::from_str(&self.single_column_aspect_ratio).ok()
    }
}

#[derive(Debug, PartialEq, Clone, Copy, Serialize)]
pub struct AspectRatio {
    pub width: f64,
    pub height: f64,
}

impl FromStr for AspectRatio {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (w, h) =
            s.split_once(':').ok_or_else(|| format!("expected 'W:H' format, got {s:?}"))?;
        let width: f64 = w.trim().parse().map_err(|_| format!("invalid width: {w:?}"))?;
        let height: f64 = h.trim().parse().map_err(|_| format!("invalid height: {h:?}"))?;
        if width <= 0.0 || height <= 0.0 {
            return Err("aspect ratio values must be positive".into());
        }
        Ok(AspectRatio { width, height })
    }
}

impl<'de> Deserialize<'de> for AspectRatio {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        AspectRatio::from_str(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(PartialConfig!)]
#[derive_args(StatusIconPartial)]
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StatusIcon {
    pub enable: bool,
}

#[derive(PartialConfig!)]
#[derive_args(StatusIconExperimentalPartial)]
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StatusIconExperimental {
    pub space_index: bool,
    pub color: bool,

    #[deprecated = "Ignored; kept for compatibility."]
    pub enable: bool,
}

#[derive(PartialConfig!)]
#[derive_args(GroupBarsPartial)]
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GroupBars {
    pub enable: bool,
    pub thickness: f64,
    pub horizontal_placement: HorizontalPlacement,
    pub vertical_placement: VerticalPlacement,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum HorizontalPlacement {
    Top,
    Bottom,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum VerticalPlacement {
    Left,
    Right,
}

impl GroupBars {
    /// Get the indicator thickness for layout space reservation
    pub fn indicator_thickness(&self) -> f64 {
        if self.enable { self.thickness } else { 0.0 }
    }
}

impl ConfigPartial {
    fn default() -> Self {
        toml::from_str(include_str!("../glide.default.toml")).unwrap()
    }

    fn validate(self) -> Result<Config, SpannedError> {
        let mut keys = Vec::new();
        for (key, cmd) in self.keys.unwrap_or_default() {
            let cmd = match cmd {
                WmCommandOrDisable::WmCommand(wm_command) => wm_command,
                WmCommandOrDisable::Disable(_) => continue,
            };
            let Ok(hotkey) = Hotkey::from_str(&key) else {
                return Err(SpannedError {
                    message: format!("Could not parse hotkey: {key}"),
                    span: None,
                });
            };
            validate_command(&cmd).map_err(|message| SpannedError {
                message: format!("keys.{key:?}: {message}"),
                span: None,
            })?;
            keys.push((hotkey, cmd));
        }
        let window_rules: Vec<WindowRule> = self
            .window_rules
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(i, rule)| {
                rule.validated().map_err(|message| SpannedError {
                    message: format!("window_rules[{i}]: {message}"),
                    span: None,
                })
            })
            .collect::<Result<_, _>>()?;
        let mut scratchpads = FxHashMap::default();
        for (i, name) in window_rules
            .iter()
            .enumerate()
            .filter_map(|(i, r)| Some((i, r.scratchpad.as_deref()?)))
        {
            if let Some(first) = scratchpads.insert(name, i) {
                return Err(SpannedError {
                    message: format!(
                        "window_rules[{i}]: scratchpad {name:?} is already used by window_rules[{first}]"
                    ),
                    span: None,
                });
            }
        }
        Ok(Config {
            settings: self.settings.validate()?,
            window_rules,
            keys,
        })
    }

    fn merge(low: Self, high: Self) -> Self {
        let include_default_keys = high.keys.is_none()
            || high.settings.default_keys.unwrap_or(Config::default().settings.default_keys);
        let mut keys = if include_default_keys {
            low.keys.unwrap_or_default()
        } else {
            Default::default()
        };
        keys.extend(high.keys.unwrap_or_default());
        Self {
            settings: SettingsPartial::merge(low.settings, high.settings),
            window_rules: high.window_rules.or(low.window_rules),
            keys: Some(keys),
        }
    }
}

impl Config {
    pub fn load(custom_path: Option<&Path>) -> anyhow::Result<Config> {
        let mut buf = String::new();
        let (mut file, path) = match custom_path {
            Some(path) => (File::open(path)?, path.to_path_buf()),
            None => {
                let mut selected: Option<(File, PathBuf)> = None;
                for path in default_config_paths() {
                    match File::open(&path) {
                        Ok(file) => {
                            selected = Some((file, path));
                            break;
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(e) => return Err(e.into()),
                    }
                }
                match selected {
                    Some(pair) => pair,
                    None => return Ok(Config::default()),
                }
            }
        };
        file.read_to_string(&mut buf)?;
        Self::parse(&buf).map_err(|e| anyhow::anyhow!("{}", format_toml_error(e, &buf, &path)))
    }

    pub fn default() -> Config {
        ConfigPartial::default().validate().unwrap()
    }

    fn parse(buf: &str) -> Result<Self, SpannedError> {
        let c: ConfigPartial = toml::from_str(buf)?;
        let defaults = ConfigPartial::default();
        ConfigPartial::merge(defaults, c).validate()
    }
}

fn format_toml_error(error: SpannedError, input: &str, path: &Path) -> String {
    use annotate_snippets::{AnnotationKind, Level, Renderer, Snippet};

    let message = error.message;
    let Some(span) = error.span else {
        return format!("could not parse config: {}", message);
    };

    let snippet = Snippet::source(input)
        .path(path.to_string_lossy())
        .annotation(AnnotationKind::Primary.span(span.start..span.end).label(message));

    let report = Level::ERROR.primary_title("could not parse config").element(snippet);

    let renderer = Renderer::styled();
    format!("{}", renderer.render(&[report]))
}

fn validate_command(cmd: &WmCommand) -> Result<(), String> {
    use crate::actor::reactor::{Command, ReactorCommand};
    let WmCommand::ReactorCommand(Command::Reactor(ReactorCommand::ToggleScratchpad {
        name,
        launch,
    })) = cmd
    else {
        return Ok(());
    };
    if name.trim().is_empty() {
        return Err("`toggle_scratchpad` name must not be empty".into());
    }
    if let Some(launch) = launch {
        crate::sys::app::validate_bundle_id(launch)
            .map_err(|e| format!("`toggle_scratchpad` launch: {e}"))?;
    }
    Ok(())
}

#[derive(Debug)]
struct SpannedError {
    message: String,
    span: Option<Range<usize>>,
}

impl From<toml::de::Error> for SpannedError {
    fn from(e: toml::de::Error) -> Self {
        Self {
            message: e.message().to_owned(),
            span: e.span(),
        }
    }
}

impl From<ValidationError> for SpannedError {
    fn from(e: ValidationError) -> Self {
        Self {
            message: format!("{e}"),
            span: None, // TODO
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::layout::LayoutCommand;
    use crate::actor::reactor::Command as ReactorCommand;
    use crate::actor::wm_controller::WmCmd;

    #[test]
    fn default_config_is_valid() {
        Config::default();
    }

    #[test]
    fn default_settings_match_unspecified_setting_values() {
        assert_eq!(Config::default().settings, Config::parse("").unwrap().settings);
    }

    #[test]
    fn scroll_gate_is_disabled_by_default() {
        assert!(!Config::default().settings.experimental.scroll.enable);
    }

    #[test]
    fn window_rules_are_empty_by_default() {
        assert!(Config::default().window_rules.is_empty());
    }

    #[test]
    fn window_rules_parse() {
        let config = Config::parse(
            r#"
            window_rules = [
              { if = { app_id = "com.example.X", title_regex = "Dialog" }, float = true },
              { if = { title_substring = "Preferences", ax_subrole = "AXDialog" }, float = true },
            ]
            "#,
        )
        .unwrap();
        assert_eq!(
            config.window_rules,
            vec![
                WindowRule {
                    conditions: WindowRuleConditions {
                        app_id: Some("com.example.X".into()),
                        title_regex: Some("Dialog".parse().unwrap()),
                        ..Default::default()
                    },
                    float: Some(true),
                    scratchpad: None,
                    frame: None,
                },
                WindowRule {
                    conditions: WindowRuleConditions {
                        title_substring: Some("Preferences".into()),
                        ax_subrole: Some("AXDialog".into()),
                        ..Default::default()
                    },
                    float: Some(true),
                    scratchpad: None,
                    frame: None,
                },
            ]
        );
    }

    #[test]
    fn window_rules_parse_with_dotted_if_keys() {
        // The `if.app_id` dotted-key form from the aerospace-style API.
        let config = Config::parse(
            r#"
            [[window_rules]]
            if.app_id = "com.example.X"
            float = true
            "#,
        )
        .unwrap();
        assert_eq!(
            config.window_rules,
            vec![WindowRule {
                conditions: WindowRuleConditions {
                    app_id: Some("com.example.X".into()),
                    ..Default::default()
                },
                float: Some(true),
                scratchpad: None,
                frame: None,
            }]
        );
    }

    #[test]
    fn window_rule_invalid_regex_is_rejected() {
        let err = Config::parse(
            r#"
            window_rules = [{ if = { title_regex = "(unterminated" }, float = true }]
            "#,
        )
        .unwrap_err();
        assert!(
            err.message.contains("regex"),
            "unexpected error: {}",
            err.message
        );
    }

    #[test]
    fn scratchpad_rule_parses_without_float() {
        let config = Config::parse(
            r#"
            [[window_rules]]
            if.app_id = "com.example.keyguard"
            scratchpad = "keyguard"
            frame = { x = 0.9, y = 0.1, width = 0.5, height = 0.8 }

            [[window_rules]]
            if.app_id = "com.example.X"
            scratchpad = "x"
            float = true
            "#,
        )
        .unwrap();
        assert_eq!(
            config.window_rules,
            vec![
                WindowRule {
                    conditions: WindowRuleConditions {
                        app_id: Some("com.example.keyguard".into()),
                        ..Default::default()
                    },
                    float: None,
                    scratchpad: Some("keyguard".into()),
                    frame: Some(FractionalRect {
                        x: 0.5,
                        y: 0.1,
                        width: 0.5,
                        height: 0.8,
                    }),
                },
                WindowRule {
                    conditions: WindowRuleConditions {
                        app_id: Some("com.example.X".into()),
                        ..Default::default()
                    },
                    float: Some(true),
                    scratchpad: Some("x".into()),
                    frame: None,
                },
            ]
        );
    }

    #[track_caller]
    fn assert_rule_rejected(rule: &str, expected: &str) {
        let err = Config::parse(&format!("[[window_rules]]\n{rule}")).unwrap_err();
        assert!(
            err.message.contains(expected),
            "unexpected error: {}",
            err.message
        );
    }

    #[test]
    fn window_rule_without_action_is_rejected() {
        assert_rule_rejected(r#"if.app_id = "com.example.X""#, "needs `float` or `scratchpad`");
    }

    #[test]
    fn scratchpad_rule_with_float_false_is_rejected() {
        assert_rule_rejected("scratchpad = \"k\"\nfloat = false", "cannot set `float = false`");
    }

    #[test]
    fn frame_without_scratchpad_is_rejected() {
        assert_rule_rejected(
            "float = true\nframe = { x = 0.1, y = 0.1, width = 0.8, height = 0.8 }",
            "`frame` requires `scratchpad`",
        );
    }

    #[test]
    fn empty_scratchpad_name_is_rejected() {
        assert_rule_rejected(r#"scratchpad = """#, "must not be empty");
    }

    #[test]
    fn blank_scratchpad_name_is_rejected() {
        assert_rule_rejected(r#"scratchpad = " \t ""#, "must not be empty");
    }

    fn parse_single_rule(rule: &str) -> WindowRule {
        let mut rules = Config::parse(&format!("[[window_rules]]\n{rule}")).unwrap().window_rules;
        assert_eq!(rules.len(), 1);
        rules.pop().unwrap()
    }

    #[test]
    fn scratchpad_rule_in_inline_array_parses() {
        let config = Config::parse(r#"window_rules = [{ scratchpad = "k" }]"#).unwrap();
        assert_eq!(
            config.window_rules,
            vec![WindowRule {
                conditions: WindowRuleConditions::default(),
                float: None,
                scratchpad: Some("k".into()),
                frame: None,
            }]
        );
    }

    #[test]
    fn scratchpad_frame_non_finite_values_are_clamped() {
        let rule = parse_single_rule(
            "scratchpad = \"k\"\nframe = { x = nan, y = -inf, width = inf, height = 0.5 }",
        );
        assert_eq!(
            rule.frame,
            Some(FractionalRect {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 0.5,
            })
        );
    }

    #[test]
    fn scratchpad_frame_negative_values_are_clamped() {
        let rule = parse_single_rule(
            "scratchpad = \"k\"\nframe = { x = -0.2, y = -1, width = -0.5, height = 0.3 }",
        );
        assert_eq!(
            rule.frame,
            Some(FractionalRect {
                x: 0.0,
                y: 0.0,
                width: 0.05,
                height: 0.3,
            })
        );
    }

    #[test]
    fn scratchpad_frame_with_unknown_field_is_rejected() {
        assert!(
            Config::parse(
                "[[window_rules]]\nscratchpad = \"k\"\n\
                 frame = { x = 0.1, y = 0.1, width = 0.8, height = 0.8, depth = 1 }"
            )
            .is_err()
        );
    }

    #[test]
    fn scratchpad_frame_with_missing_field_is_rejected() {
        assert!(
            Config::parse(
                "[[window_rules]]\nscratchpad = \"k\"\nframe = { x = 0.1, y = 0.1, width = 0.8 }"
            )
            .is_err()
        );
    }

    #[test]
    fn scratchpad_rule_with_float_true_and_frame_is_accepted() {
        let rule = parse_single_rule(
            "scratchpad = \"k\"\nfloat = true\nframe = { x = 0.2, y = 0.1, width = 0.6, height = 0.8 }",
        );
        assert_eq!(rule.float, Some(true));
        assert_eq!(rule.scratchpad.as_deref(), Some("k"));
        assert_eq!(
            rule.frame,
            Some(FractionalRect {
                x: 0.2,
                y: 0.1,
                width: 0.6,
                height: 0.8,
            })
        );
    }

    #[test]
    fn frame_with_float_false_and_no_scratchpad_is_rejected() {
        assert_rule_rejected(
            "float = false\nframe = { x = 0.1, y = 0.1, width = 0.8, height = 0.8 }",
            "`frame` requires `scratchpad`",
        );
    }

    #[test]
    fn frame_alone_is_rejected() {
        let err = Config::parse(
            "[[window_rules]]\nframe = { x = 0.1, y = 0.1, width = 0.8, height = 0.8 }",
        )
        .unwrap_err();
        assert!(err.message.starts_with("window_rules[0]: "), "{}", err.message);
    }

    #[test]
    fn empty_scratchpad_name_with_float_false_is_rejected() {
        assert!(Config::parse("[[window_rules]]\nscratchpad = \"\"\nfloat = false").is_err());
    }

    #[test]
    fn non_string_scratchpad_name_is_rejected() {
        assert!(Config::parse("[[window_rules]]\nscratchpad = 5").is_err());
        assert!(Config::parse("[[window_rules]]\nscratchpad = true").is_err());
    }

    #[test]
    fn window_rule_error_names_the_invalid_rule() {
        let err = Config::parse(
            r#"
            [[window_rules]]
            float = true

            [[window_rules]]
            if.app_id = "com.example.X"
            "#,
        )
        .unwrap_err();
        assert!(
            err.message.starts_with("window_rules[1]: "),
            "unexpected error: {}",
            err.message
        );
    }

    #[test]
    fn legacy_float_rules_still_parse_after_scratchpad_rules() {
        let config = Config::parse(
            r#"
            [[window_rules]]
            if.app_id = "com.example.K"
            scratchpad = "k"

            [[window_rules]]
            if.app_id = "com.example.X"
            float = false
            "#,
        )
        .unwrap();
        assert_eq!(config.window_rules.len(), 2);
        assert_eq!(config.window_rules[1].float, Some(false));
        assert_eq!(config.window_rules[1].scratchpad, None);
        assert_eq!(config.window_rules[1].frame, None);
    }

    #[test]
    fn two_rules_with_the_same_scratchpad_name_are_rejected() {
        let err = Config::parse(
            r#"
            [[window_rules]]
            if.app_id = "com.example.A"
            scratchpad = "k"

            [[window_rules]]
            if.app_id = "com.example.X"
            float = true

            [[window_rules]]
            if.app_id = "com.example.B"
            scratchpad = "k"
            frame = { x = 0, y = 0, width = 0.5, height = 0.5 }
            "#,
        )
        .unwrap_err();
        assert_eq!(
            err.message,
            r#"window_rules[2]: scratchpad "k" is already used by window_rules[0]"#
        );
    }

    #[test]
    fn rules_with_different_scratchpad_names_are_accepted() {
        let config = Config::parse(
            r#"
            [[window_rules]]
            if.app_id = "com.example.A"
            scratchpad = "a"

            [[window_rules]]
            if.app_id = "com.example.B"
            scratchpad = "b"
            "#,
        )
        .unwrap();
        assert_eq!(config.window_rules.len(), 2);
    }

    fn parse_toggle(args: &str) -> Result<Config, SpannedError> {
        Config::parse(&format!(
            "[keys]\n\"Meta + Alt + K\" = {{ toggle_scratchpad = {{ {args} }} }}"
        ))
    }

    #[test]
    fn toggle_scratchpad_with_blank_name_is_rejected() {
        for name in ["", "  \t"] {
            let err = parse_toggle(&format!("name = {name:?}")).unwrap_err();
            assert!(err.message.contains("name must not be empty"), "{}", err.message);
            assert!(
                err.message.starts_with(r#"keys."Meta + Alt + K": "#),
                "{}",
                err.message
            );
        }
    }

    #[test]
    fn toggle_scratchpad_with_invalid_launch_is_rejected() {
        for launch in ["", "-a", "com.example app", "com.example;rm", "$(id)"] {
            let err = parse_toggle(&format!("name = \"k\", launch = {launch:?}")).unwrap_err();
            assert!(
                err.message.contains("`toggle_scratchpad` launch"),
                "{launch:?}: {}",
                err.message
            );
        }
    }

    #[test]
    fn toggle_scratchpad_with_valid_launch_is_accepted() {
        parse_toggle(r#"name = "k", launch = "com.example.my-app2""#).unwrap();
        parse_toggle(r#"name = "k""#).unwrap();
    }

    #[test]
    fn toggle_scratchpad_without_name_is_rejected() {
        assert!(
            Config::parse(
                r#"
                [keys]
                "Meta + Alt + K" = { toggle_scratchpad = { launch = "com.x" } }
                "#,
            )
            .is_err()
        );
    }

    #[test]
    fn toggle_scratchpad_with_unknown_field_is_rejected() {
        assert!(
            Config::parse(
                r#"
                [keys]
                "Meta + Alt + K" = { toggle_scratchpad = { name = "k", lanch = "com.x" } }
                "#,
            )
            .is_err()
        );
    }

    #[test]
    fn toggle_scratchpad_round_trips_through_serde() {
        let cmd = crate::actor::reactor::ReactorCommand::ToggleScratchpad {
            name: "k".into(),
            launch: Some("com.x".into()),
        };
        let text = ron::to_string(&ReactorCommand::Reactor(cmd)).unwrap();
        let parsed: ReactorCommand = ron::from_str(&text).unwrap();
        let ReactorCommand::Reactor(crate::actor::reactor::ReactorCommand::ToggleScratchpad {
            name,
            launch,
        }) = parsed
        else {
            panic!("unexpected command: {parsed:?} from {text}");
        };
        assert_eq!(name, "k");
        assert_eq!(launch.as_deref(), Some("com.x"));
    }

    #[test]
    fn toggle_scratchpad_command_parses() {
        let config = Config::parse(
            r#"
            [settings]
            default_keys = false

            [keys]
            "Meta + Alt + K" = { toggle_scratchpad = { name = "k", launch = "com.x" } }
            "Meta + Alt + J" = { toggle_scratchpad = { name = "j" } }
            "#,
        )
        .unwrap();
        let cmd = |key: &str| {
            &config
                .keys
                .iter()
                .find(|(hk, _)| hk.to_string() == key)
                .unwrap_or_else(|| panic!("{key} should be present"))
                .1
        };
        let WmCommand::ReactorCommand(ReactorCommand::Reactor(
            crate::actor::reactor::ReactorCommand::ToggleScratchpad { name, launch },
        )) = cmd("Alt + Meta + KeyK")
        else {
            panic!("unexpected command: {:?}", cmd("Alt + Meta + KeyK"));
        };
        assert_eq!(name, "k");
        assert_eq!(launch.as_deref(), Some("com.x"));

        let WmCommand::ReactorCommand(ReactorCommand::Reactor(
            crate::actor::reactor::ReactorCommand::ToggleScratchpad { name, launch },
        )) = cmd("Alt + Meta + KeyJ")
        else {
            panic!("unexpected command: {:?}", cmd("Alt + Meta + KeyJ"));
        };
        assert_eq!(name, "j");
        assert_eq!(launch, &None);
    }

    /// The commented-out `toggle_scratchpad` key and the example window rules
    /// in the default config, uncommented.
    fn default_config_scratchpad_examples() -> String {
        let text = include_str!("../glide.default.toml");
        let key = text
            .lines()
            .filter_map(|line| line.strip_prefix("# \""))
            .find(|line| line.contains("toggle_scratchpad ="))
            .expect("an example key with toggle_scratchpad");
        let mut examples = format!("[keys]\n\"{key}\n");
        let mut in_rule = false;
        for line in text.lines() {
            if line == "#   [[window_rules]]" {
                in_rule = true;
            }
            match line.strip_prefix("#   ") {
                Some(rule_line) if in_rule => examples.extend([rule_line, "\n"]),
                _ => in_rule = false,
            }
        }
        examples
    }

    #[test]
    fn default_config_scratchpad_examples_parse() {
        let examples = default_config_scratchpad_examples();
        let config = Config::parse(&examples).unwrap_or_else(|e| panic!("{e:?}\n{examples}"));

        let commands: Vec<_> = config
            .keys
            .iter()
            .filter_map(|(key, cmd)| match cmd {
                WmCommand::ReactorCommand(ReactorCommand::Reactor(
                    crate::actor::reactor::ReactorCommand::ToggleScratchpad { name, launch },
                )) => Some((key.to_string(), name.as_str(), launch.as_deref())),
                _ => None,
            })
            .collect();
        assert_eq!(
            commands,
            [(
                "Alt + Meta + KeyK".to_owned(),
                "keyguard",
                Some("com.artemchep.keyguard")
            )]
        );

        assert!(config.window_rules.len() > 1, "{examples}");
        let scratchpad_rules: Vec<_> =
            config.window_rules.iter().filter(|rule| rule.scratchpad.is_some()).collect();
        assert_eq!(
            scratchpad_rules,
            [&WindowRule {
                conditions: WindowRuleConditions {
                    app_id: Some("com.artemchep.keyguard".into()),
                    ..Default::default()
                },
                float: None,
                scratchpad: Some("keyguard".into()),
                frame: Some(FractionalRect {
                    x: 0.2,
                    y: 0.1,
                    width: 0.6,
                    height: 0.8,
                }),
            }]
        );
    }

    #[test]
    fn default_keys_exclude_scroll_experimental_commands() {
        let config = Config::default();
        assert!(!config.keys.iter().any(|(_, cmd)| {
            matches!(
                cmd,
                WmCommand::ReactorCommand(ReactorCommand::Layout(
                    LayoutCommand::ChangeLayoutKind
                        | LayoutCommand::ToggleColumnTabbed
                        | LayoutCommand::CycleColumnWidth
                ))
            )
        }));
    }

    #[test]
    fn default_keys_false_excludes_default_bindings() {
        let config = Config::parse(
            r#"
            [settings]
            default_keys = false

            [keys]
            "Alt + Q" = "debug"
            "#,
        )
        .unwrap();

        // Should only have our custom key, not the defaults
        assert_eq!(config.keys.len(), 1);
        let (hotkey, _cmd) = &config.keys[0];
        assert_eq!(hotkey.to_string(), "Alt + KeyQ");
    }

    #[test]
    fn default_keys_true_includes_default_bindings() {
        let config = Config::parse(
            r#"
            [settings]
            default_keys = true

            [keys]
            "Alt + Q" = "debug"
            "#,
        )
        .unwrap();

        // Should have default keys plus our custom key
        let default_key_count = Config::default().keys.len();
        assert_eq!(config.keys.len(), default_key_count + 1);

        // Our custom key should be present
        assert!(config.keys.iter().any(|(hk, _)| hk.to_string() == "Alt + KeyQ"));
    }

    #[test]
    fn missing_keys_section_includes_default_bindings() {
        let config = Config::parse(
            r#"
            [settings]
            animate = false
            "#,
        )
        .unwrap();

        assert_eq!(config.keys.len(), Config::default().keys.len());
    }

    #[test]
    fn disable_removes_key_binding() {
        let config = Config::parse(
            r#"
            [settings]
            default_keys = false

            [keys]
            "Alt + Q" = "debug"
            "Alt + W" = "disable"
            "#,
        )
        .unwrap();

        // "disable" key should not appear in final config
        assert_eq!(config.keys.len(), 1);
        assert!(config.keys.iter().any(|(hk, _)| hk.to_string() == "Alt + KeyQ"));
        assert!(!config.keys.iter().any(|(hk, _)| hk.to_string() == "Alt + KeyW"));
    }

    #[test]
    fn disable_can_override_default_key() {
        // First verify Alt+H exists in defaults
        let default_config = Config::default();
        assert!(
            default_config.keys.iter().any(|(hk, _)| hk.to_string() == "Alt + KeyH"),
            "Alt+H should be a default key binding"
        );

        let config = Config::parse(
            r#"
            [settings]
            default_keys = true

            [keys]
            "Alt + H" = "disable"
            "#,
        )
        .unwrap();

        // Alt+H should be removed even though it's in defaults
        assert!(!config.keys.iter().any(|(hk, _)| hk.to_string() == "Alt + KeyH"));
        // But other default keys should still be present
        assert!(config.keys.iter().any(|(hk, _)| hk.to_string() == "Alt + KeyJ"));
    }

    #[test]
    fn exec_cmd_options_parse() {
        let config = Config::parse(
            r#"
            [settings]
            default_keys = false

            [keys]
            "Alt + Q" = { exec = ["bash", "-c", "echo hi"] }
            "Alt + W" = { exec = { cmd = ["bash", "-c", "echo hi"], unsafe_privileged = true } }
            "#,
        )
        .unwrap();

        let cmd_q = &config
            .keys
            .iter()
            .find(|(hk, _)| hk.to_string() == "Alt + KeyQ")
            .expect("Alt + KeyQ should be present")
            .1;
        let WmCommand::Wm(WmCmd::Exec(exec_cmd)) = cmd_q else {
            panic!("Expected exec command; got {cmd_q:?}");
        };
        assert_eq!(
            exec_cmd.clone().normalize(),
            crate::actor::wm_controller::NormalizedExecCmd {
                cmd_args: vec!["bash".to_owned(), "-c".to_owned(), "echo hi".to_owned()],
                unsafe_privileged: false,
            }
        );

        let cmd_w = &config
            .keys
            .iter()
            .find(|(hk, _)| hk.to_string() == "Alt + KeyW")
            .expect("Alt + KeyW should be present")
            .1;
        let WmCommand::Wm(WmCmd::Exec(exec_cmd)) = cmd_w else {
            panic!("Expected exec command; got {cmd_w:?}");
        };
        assert_eq!(
            exec_cmd.clone().normalize(),
            crate::actor::wm_controller::NormalizedExecCmd {
                cmd_args: vec!["bash".to_owned(), "-c".to_owned(), "echo hi".to_owned()],
                unsafe_privileged: true,
            }
        );
    }

    #[test]
    fn aspect_ratio_from_str_valid() {
        let ar = AspectRatio::from_str("16:9").unwrap();
        assert_eq!(ar.width, 16.0);
        assert_eq!(ar.height, 9.0);
    }

    #[test]
    fn aspect_ratio_from_str_with_spaces() {
        let ar = AspectRatio::from_str(" 4 : 3 ").unwrap();
        assert_eq!(ar.width, 4.0);
        assert_eq!(ar.height, 3.0);
    }

    #[test]
    fn aspect_ratio_from_str_invalid() {
        assert!(AspectRatio::from_str("16x9").is_err());
        assert!(AspectRatio::from_str("0:9").is_err());
        assert!(AspectRatio::from_str("16:-1").is_err());
        assert!(AspectRatio::from_str("abc:def").is_err());
    }

    #[test]
    fn arrow_keys_parse_correctly() {
        let config = Config::parse(
            r#"
            [settings]
            default_keys = false

            [keys]
            "Alt + ArrowLeft" = { move_focus = "left" }
            "Alt + ArrowDown" = { move_focus = "down" }
            "Alt + ArrowUp" = { move_focus = "up" }
            "Alt + ArrowRight" = { move_focus = "right" }
            "#,
        )
        .unwrap();

        // Should have all 4 arrow key bindings
        assert_eq!(config.keys.len(), 4);

        // Verify all arrow keys are present
        assert!(config.keys.iter().any(|(hk, _)| hk.to_string() == "Alt + ArrowLeft"));
        assert!(config.keys.iter().any(|(hk, _)| hk.to_string() == "Alt + ArrowDown"));
        assert!(config.keys.iter().any(|(hk, _)| hk.to_string() == "Alt + ArrowUp"));
        assert!(config.keys.iter().any(|(hk, _)| hk.to_string() == "Alt + ArrowRight"));
    }
}
