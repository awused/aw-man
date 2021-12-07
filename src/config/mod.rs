use std::cmp::max;
use std::convert::TryFrom;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use gtk::gdk;
use once_cell::sync::Lazy;
use serde::{de, Deserialize, Deserializer};
use structopt::StructOpt;

use crate::com::Res;

#[derive(Debug, StructOpt)]
#[structopt(name = "aw-man", about = "Awused's manga and image viewer.")]
pub struct Opt {
    #[structopt(short, long)]
    pub debug: bool,

    #[structopt(short, long)]
    /// Start in manga mode.
    pub manga: bool,

    #[structopt(short, long)]
    /// Start in upscaling mode. Not yet supported.
    pub upscale: bool,

    #[structopt(long)]
    /// Print the supported file extensions and exit.
    show_supported: bool,

    #[structopt(short, long, parse(from_os_str))]
    awconf: Option<PathBuf>,

    #[structopt(parse(from_os_str))]
    file_name: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
pub struct Shortcut {
    pub action: String,
    pub key: String,
    pub modifiers: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ContextMenuEntry {
    pub action: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct Config {
    pub target_resolution: String,
    #[serde(default, deserialize_with = "empty_string_is_none")]
    pub minimum_resolution: Option<String>,

    #[serde(default, deserialize_with = "empty_path_is_none")]
    pub temp_directory: Option<PathBuf>,

    #[serde(deserialize_with = "assert_non_negative")]
    pub preload_ahead: isize,
    #[serde(deserialize_with = "assert_non_negative")]
    pub preload_behind: isize,

    #[serde(default, deserialize_with = "empty_string_is_none")]
    pub background_colour: Option<gdk::RGBA>,

    #[serde(default = "three_hundred", deserialize_with = "assert_positive")]
    pub scroll_amount: u32,

    #[serde(default)]
    pub shortcuts: Vec<Shortcut>,
    #[serde(default)]
    pub context_menu: Vec<ContextMenuEntry>,

    #[serde(default)]
    pub allow_external_extractors: bool,

    #[serde(default)]
    pub use_sofware_renderer: bool,

    #[serde(default = "two", deserialize_with = "assert_positive")]
    pub extraction_threads: usize,
    #[serde(default = "half_threads", deserialize_with = "assert_positive")]
    pub loading_threads: usize,
    #[serde(default = "one", deserialize_with = "assert_positive")]
    pub upscaling_threads: usize,
    #[serde(default = "half_threads_four", deserialize_with = "assert_positive")]
    pub downscaling_threads: usize,

    #[serde(default, deserialize_with = "assert_non_negative")]
    pub prescale: isize,
    // #[serde(default)]
    // maximum_upscaled: u32,
    #[serde(default, deserialize_with = "empty_path_is_none")]
    pub alternate_upscaler: Option<PathBuf>,
    #[serde(default, deserialize_with = "empty_path_is_none")]
    pub socket_dir: Option<PathBuf>,
}

const fn two() -> usize {
    2
}

const fn one() -> usize {
    1
}

const fn three_hundred() -> u32 {
    300
}

fn half_threads() -> usize {
    max(num_cpus::get() / 2, 2)
}

fn half_threads_four() -> usize {
    max(num_cpus::get() / 2, 4)
}

// Serde seems broken with OsString for some reason
fn empty_path_is_none<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: From<PathBuf>,
{
    let s = PathBuf::deserialize(deserializer)?;
    if s.as_os_str().is_empty() {
        Ok(None)
    } else {
        Ok(Some(s.into()))
    }
}

fn empty_string_is_none<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr,
    <T as FromStr>::Err: fmt::Debug,
{
    let s = String::deserialize(deserializer)?;
    if s.is_empty() {
        Ok(None)
    } else {
        match FromStr::from_str(&s) {
            Ok(v) => Ok(Some(v)),
            Err(e) => Err(de::Error::custom(format!("{:?}", e))),
        }
    }
}

fn assert_positive<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: TryFrom<isize>,
    <T as TryFrom<isize>>::Error: fmt::Debug,
{
    let v = isize::deserialize(deserializer)?;
    if v > 0 {
        Ok(T::try_from(v).expect("Number too big"))
    } else {
        Err(de::Error::custom("thread counts must be greater than zero"))
    }
}

fn assert_non_negative<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: TryFrom<isize>,
    <T as TryFrom<isize>>::Error: fmt::Debug,
{
    let v = isize::deserialize(deserializer)?;
    if v >= 0 {
        Ok(T::try_from(v).expect("Number too big"))
    } else {
        Err(de::Error::custom("Negative numbers not allowed"))
    }
}

pub static OPTIONS: Lazy<Opt> = Lazy::new(Opt::from_args);

pub static CONFIG: Lazy<Config> =
    Lazy::new(|| awconf::load_config::<Config>("aw-man", &OPTIONS.awconf).unwrap());

pub static TARGET_RES: Lazy<Res> = Lazy::new(|| {
    let split = CONFIG.target_resolution.splitn(2, 'x');
    let split: Vec<&str> = split.collect();
    if let [a, b] = split[..] {
        let a = a.parse::<u32>();
        let b = b.parse::<u32>();
        if let (Ok(w), Ok(h)) = (a, b) {
            return (w, h).into();
        }
    }
    panic!(
        "target_resolution must be of the form WIDTHxHEIGHT, use 0x0 to disable. Example: \
         3840x2160"
    )
});

pub static MINIMUM_RES: Lazy<Res> = Lazy::new(|| {
    if let Some(minres) = &CONFIG.minimum_resolution {
        let split = minres.splitn(2, 'x');
        let split: Vec<&str> = split.collect();
        if let [a, b] = split[..] {
            let a = a.parse::<u32>();
            let b = b.parse::<u32>();
            if let (Ok(w), Ok(h)) = (a, b) {
                return (w, h).into();
            }
        }
        panic!("minimum_resolution must be of the form WIDTHxHEIGHT. Example: 3840x2160")
    } else {
        (0, 0).into()
    }
});

pub static FILE_NAME: Lazy<&PathBuf> = Lazy::new(|| {
    OPTIONS
        .file_name
        .as_ref()
        .expect("File name must be specified.")
});

pub fn init() -> bool {
    Lazy::force(&OPTIONS);
    Lazy::force(&CONFIG);
    Lazy::force(&TARGET_RES);
    Lazy::force(&MINIMUM_RES);

    if OPTIONS.show_supported {
        return false;
    }

    Lazy::force(&FILE_NAME);

    if CONFIG.use_sofware_renderer {
        std::env::set_var("GSK_RENDERER", "cairo");
    }

    true
}
