use std::cmp::max;
use std::convert::TryFrom;
use std::fmt;
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::str::FromStr;
use std::thread::available_parallelism;

use clap::Parser;
use gtk::gdk;
use once_cell::sync::Lazy;
use serde::{Deserialize, Deserializer, de};

use crate::com::{DisplayMode, Fit, Res};

#[derive(Debug, Parser)]
#[command(name = "aw-man", about = "Awused's manga and image viewer.")]
pub struct Opt {
    #[arg(short, long)]
    /// Start in manga mode.
    pub manga: bool,

    #[arg(short, long)]
    /// Start in upscaling mode.
    pub upscale: bool,

    #[arg(long, value_enum, default_value_t)]
    /// The initial page fit mode. Matches values from AWMAN_FIT_MODE.
    pub fit: Fit,

    #[arg(long, value_enum, default_value_t)]
    /// The initial display mode. Matches values from AWMAN_DISPLAY_MODE.
    pub display: DisplayMode,

    #[arg(short, long)]
    /// Always open in fileset mode instead of directory mode.
    pub fileset: bool,

    #[arg(long)]
    /// A single command to run immediately on startup. Can be repeated.
    pub command: Vec<String>,

    #[arg(short, long, value_parser, value_name = "FILE")]
    /// Alternate config file to use or /dev/null to use the default.
    awconf: Option<PathBuf>,

    #[arg(long)]
    /// Print the supported file extensions and exit.
    pub show_supported: bool,

    #[arg(long)]
    /// Print the supported GPUs for OpenCL downscaling and exit.
    pub show_gpus: bool,

    #[arg(value_parser)]
    pub file_names: Vec<PathBuf>,
}

#[derive(Debug, Deserialize)]
pub struct Shortcut {
    pub action: String,
    pub key: String,
    pub modifiers: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContextMenuGroup {
    Section(String),
    Submenu(String),
}

#[derive(Debug, Deserialize)]
pub struct ContextMenuEntry {
    pub action: String,
    pub name: String,
    #[serde(default, flatten)]
    pub group: Option<ContextMenuGroup>,
}

#[derive(Debug, Deserialize)]
pub struct Config {
    pub target_resolution: Res,
    #[serde(default, deserialize_with = "empty_string_is_none")]
    pub minimum_resolution: Option<Res>,

    #[serde(default, deserialize_with = "empty_path_is_none")]
    pub temp_directory: Option<PathBuf>,

    #[serde(default)]
    pub preload_ahead: usize,
    #[serde(default)]
    pub preload_behind: usize,

    #[serde(default, deserialize_with = "empty_string_is_none")]
    pub background_colour: Option<gdk::RGBA>,

    #[serde(default = "three_hundred")]
    pub scroll_amount: NonZeroU32,
    #[serde(default)]
    pub scroll_duration: u32,

    #[serde(default, deserialize_with = "zero_is_none")]
    pub idle_timeout: Option<NonZeroU64>,

    #[serde(default)]
    pub shortcuts: Vec<Shortcut>,
    #[serde(default)]
    pub context_menu: Vec<ContextMenuEntry>,

    #[serde(default)]
    pub gpu_prefix: String,

    #[serde(default, deserialize_with = "zero_is_none")]
    pub gpu_vram_limit_gb: Option<NonZeroU16>,

    #[serde(default)]
    pub allow_external_extractors: bool,

    #[serde(default, deserialize_with = "empty_string_is_none")]
    pub startup_command: Option<String>,
    #[serde(default, deserialize_with = "empty_string_is_none")]
    pub page_change_command: Option<String>,
    #[serde(default = "five", deserialize_with = "zero_is_none")]
    pub page_change_debounce: Option<NonZeroU16>,
    #[serde(default, deserialize_with = "empty_string_is_none")]
    pub archive_change_command: Option<String>,
    #[serde(default, deserialize_with = "empty_string_is_none")]
    pub idle_command: Option<String>,
    #[serde(default, deserialize_with = "empty_string_is_none")]
    pub unidle_command: Option<String>,
    #[serde(default, deserialize_with = "empty_string_is_none")]
    pub mode_change_command: Option<String>,
    #[serde(default, deserialize_with = "empty_string_is_none")]
    pub quit_command: Option<String>,

    #[serde(default, deserialize_with = "empty_path_is_none")]
    pub alternate_upscaler: Option<PathBuf>,

    // TODO -- with preloading this is probably unnecessary
    #[serde(default)]
    pub force_rgba: bool,

    #[serde(default)]
    pub prescale: usize,
    #[serde(default, deserialize_with = "zero_is_none")]
    pub max_strip_preload_ahead: Option<NonZeroUsize>,
    #[serde(default, deserialize_with = "empty_path_is_none")]
    pub socket_dir: Option<PathBuf>,

    #[serde(default, deserialize_with = "zero_is_none")]
    pub upscale_timeout: Option<NonZeroU64>,

    #[serde(default = "two")]
    pub extraction_threads: NonZeroUsize,
    #[serde(default = "half_threads")]
    pub loading_threads: NonZeroUsize,
    #[serde(default = "one")]
    pub upscaling_threads: NonZeroUsize,
    #[serde(default = "half_threads_four")]
    pub downscaling_threads: NonZeroUsize,
}

const fn one() -> NonZeroUsize {
    NonZeroUsize::new(1).unwrap()
}

const fn two() -> NonZeroUsize {
    NonZeroUsize::new(2).unwrap()
}

const fn five() -> Option<NonZeroU16> {
    NonZeroU16::new(5)
}

const fn three_hundred() -> NonZeroU32 {
    NonZeroU32::new(300).unwrap()
}

fn half_threads() -> NonZeroUsize {
    NonZeroUsize::new(max(
        available_parallelism().map(NonZeroUsize::get).unwrap_or_default() / 2,
        2,
    ))
    .unwrap()
}

fn half_threads_four() -> NonZeroUsize {
    NonZeroUsize::new(max(
        available_parallelism().map(NonZeroUsize::get).unwrap_or_default() / 2,
        4,
    ))
    .unwrap()
}

// Serde seems broken with OsString for some reason
fn empty_path_is_none<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: From<PathBuf>,
{
    let s = PathBuf::deserialize(deserializer)?;
    if s.as_os_str().is_empty() { Ok(None) } else { Ok(Some(s.into())) }
}

fn empty_string_is_none<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr,
    <T as FromStr>::Err: fmt::Debug,
{
    let s = <String>::deserialize(deserializer)?;
    if s.is_empty() {
        Ok(None)
    } else {
        match FromStr::from_str(&s) {
            Ok(v) => Ok(Some(v)),
            Err(e) => Err(de::Error::custom(format!("{e:?}"))),
        }
    }
}

fn zero_is_none<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: TryFrom<NonZeroU64>,
    <T as TryFrom<NonZeroU64>>::Error: fmt::Display,
{
    let u = u64::deserialize(deserializer)?;
    if let Some(u) = NonZeroU64::new(u) {
        match T::try_from(u) {
            Ok(v) => Ok(Some(v)),
            Err(e) => Err(de::Error::custom(format!("{e}"))),
        }
    } else {
        Ok(None)
    }
}

pub static OPTIONS: Lazy<Opt> = Lazy::new(Opt::parse);

static DEFAULT_CONFIG: &str = include_str!("../../aw-man.toml.sample");

pub static CONFIG: Lazy<Config> = Lazy::new(|| {
    match awconf::load_config::<Config>("aw-man", OPTIONS.awconf.as_ref(), Some(DEFAULT_CONFIG)) {
        Ok((conf, Some(path))) => {
            info!("Loaded config from {path:?}");
            conf
        }
        Ok((conf, None)) => {
            info!("Loaded default config");
            conf
        }
        Err(e) => {
            error!("Error loading config: {e}");
            panic!("Error loading config: {e}");
        }
    }
});

pub fn init() {
    Lazy::force(&OPTIONS);
    Lazy::force(&CONFIG);
}
