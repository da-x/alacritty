use std::path::PathBuf;

use log::error;
use serde::{Deserialize, Deserializer};

use alacritty_config_derive::ConfigDeserialize;
use alacritty_terminal::config::{Percentage, LOG_TARGET_CONFIG};
use crossfont::Size as FontSize;
use crate::config::bindings::{self, Binding, KeyBinding, MouseBinding};
use crate::config::debug::Debug;
use crate::config::font::{Size, Font};
use crate::config::mouse::Mouse;
use crate::config::window::WindowConfig;

#[derive(ConfigDeserialize, Debug, PartialEq, Default)]
pub struct SmallFontSize {
    pub size: Size,
}

#[derive(ConfigDeserialize, Debug, PartialEq, Default)]
pub struct SmallFontConfig {
    font: Font,

    // Font size in points
    pub upper_bound: Option<SmallFontSize>,
}

impl SmallFontConfig {
    pub fn check_bound(&self, size: FontSize) -> Option<&Font> {
        match &self.upper_bound {
            Some(upper_bound) => {
                if size <= upper_bound.size.0 {
                    Some(&self.font)
                } else {
                    None
                }
            }
            None => None,
        }
    }
}

#[derive(ConfigDeserialize, Debug, PartialEq)]
pub struct UIConfig {
    /// Font configuration.
    pub font: Font,

    /// Font configuration
    small_font: Option<SmallFontConfig>,

    /// Window configuration.
    pub window: WindowConfig,

    pub mouse: Mouse,

    /// Debug options.
    pub debug: Debug,

    /// Send escape sequences using the alt key.
    pub alt_send_esc: bool,

    /// Live config reload.
    pub live_config_reload: bool,

    /// Path where config was loaded from.
    #[config(skip)]
    pub config_paths: Vec<PathBuf>,

    /// Keybindings.
    key_bindings: KeyBindings,

    /// Bindings for the mouse.
    mouse_bindings: MouseBindings,

    /// Background opacity from 0.0 to 1.0.
    background_opacity: Percentage,
}

impl Default for UIConfig {
    fn default() -> Self {
        Self {
            alt_send_esc: true,
            live_config_reload: true,
            font: Default::default(),
            window: Default::default(),
            mouse: Default::default(),
            debug: Default::default(),
            small_font: Default::default(),
            config_paths: Default::default(),
            key_bindings: Default::default(),
            mouse_bindings: Default::default(),
            background_opacity: Default::default(),
        }
    }
}

impl UIConfig {
    #[inline]
    pub fn background_opacity(&self) -> f32 {
        self.background_opacity.as_f32()
    }

    #[inline]
    pub fn key_bindings(&self) -> &[KeyBinding] {
        &self.key_bindings.0.as_slice()
    }

    /// Return font to use
    #[inline]
    pub fn font(&self, size: &FontSize) -> &Font {
        match &self.small_font {
            &None => &self.font,
            &Some(ref small_font_config) => match small_font_config.check_bound(*size) {
                None => &self.font,
                Some(small_font) => small_font,
            },
        }
    }

    /// Return the basic font to use
    #[inline]
    pub fn basic_font(&self) -> &Font {
        &self.font
    }

    #[inline]
    pub fn mouse_bindings(&self) -> &[MouseBinding] {
        self.mouse_bindings.0.as_slice()
    }
}

#[derive(Debug, PartialEq)]
struct KeyBindings(Vec<KeyBinding>);

impl Default for KeyBindings {
    fn default() -> Self {
        Self(bindings::default_key_bindings())
    }
}

impl<'de> Deserialize<'de> for KeyBindings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self(deserialize_bindings(deserializer, Self::default().0)?))
    }
}

#[derive(Debug, PartialEq)]
struct MouseBindings(Vec<MouseBinding>);

impl Default for MouseBindings {
    fn default() -> Self {
        Self(bindings::default_mouse_bindings())
    }
}

impl<'de> Deserialize<'de> for MouseBindings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self(deserialize_bindings(deserializer, Self::default().0)?))
    }
}

fn deserialize_bindings<'a, D, T>(
    deserializer: D,
    mut default: Vec<Binding<T>>,
) -> Result<Vec<Binding<T>>, D::Error>
where
    D: Deserializer<'a>,
    T: Copy + Eq,
    Binding<T>: Deserialize<'a>,
{
    let values = Vec::<serde_yaml::Value>::deserialize(deserializer)?;

    // Skip all invalid values.
    let mut bindings = Vec::with_capacity(values.len());
    for value in values {
        match Binding::<T>::deserialize(value) {
            Ok(binding) => bindings.push(binding),
            Err(err) => {
                error!(target: LOG_TARGET_CONFIG, "Config error: {}; ignoring binding", err);
            },
        }
    }

    // Remove matching default bindings.
    for binding in bindings.iter() {
        default.retain(|b| !b.triggers_match(binding));
    }

    bindings.extend(default);

    Ok(bindings)
}

/// A delta for a point in a 2 dimensional plane.
#[derive(ConfigDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Delta<T: Default> {
    /// Horizontal change.
    pub x: T,
    /// Vertical change.
    pub y: T,
}
