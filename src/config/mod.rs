use std::cmp::max;
use std::convert::TryFrom;
use std::fmt;
use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::str::FromStr;

use clap::StructOpt;
use gtk::gdk;
use once_cell::sync::Lazy;
use serde::{de, Deserialize, Deserializer};

use crate::com::Res;
use crate::manager::files::print_formats;

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

    #[serde()]
    pub preload_ahead: usize,
    #[serde()]
    pub preload_behind: usize,

    #[serde(default, deserialize_with = "empty_string_is_none")]
    pub background_colour: Option<gdk::RGBA>,

    #[serde(default = "three_hundred")]
    pub scroll_amount: NonZeroU32,

    #[serde(default, deserialize_with = "zero_is_none")]
    pub upscale_timeout: Option<NonZeroU64>,

    #[serde(default, deserialize_with = "zero_is_none")]
    pub idle_timeout: Option<NonZeroU64>,

    #[serde(default)]
    pub shortcuts: Vec<Shortcut>,
    #[serde(default)]
    pub context_menu: Vec<ContextMenuEntry>,

    #[serde(default)]
    pub allow_external_extractors: bool,

    #[serde(default)]
    pub use_sofware_renderer: bool,

    #[serde(default = "two")]
    pub extraction_threads: NonZeroUsize,
    #[serde(default = "half_threads")]
    pub loading_threads: NonZeroUsize,
    #[serde(default = "one")]
    pub upscaling_threads: NonZeroUsize,
    #[serde(default = "half_threads_four")]
    pub downscaling_threads: NonZeroUsize,

    #[serde(default)]
    pub prescale: usize,
    // #[serde(default)]
    // maximum_upscaled: u32,
    #[serde(default, deserialize_with = "empty_path_is_none")]
    pub alternate_upscaler: Option<PathBuf>,
    #[serde(default, deserialize_with = "empty_path_is_none")]
    pub socket_dir: Option<PathBuf>,
}

fn one() -> NonZeroUsize {
    NonZeroUsize::new(1).unwrap()
}

fn two() -> NonZeroUsize {
    NonZeroUsize::new(2).unwrap()
}

fn three_hundred() -> NonZeroU32 {
    NonZeroU32::new(300).unwrap()
}

fn half_threads() -> NonZeroUsize {
    NonZeroUsize::new(max(num_cpus::get() / 2, 2)).unwrap()
}

fn half_threads_four() -> NonZeroUsize {
    NonZeroUsize::new(max(num_cpus::get() / 2, 4)).unwrap()
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

fn zero_is_none<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: TryFrom<u64>,
    <T as TryFrom<u64>>::Error: fmt::Display,
{
    let u = u64::deserialize(deserializer)?;
    if u == 0 {
        Ok(None)
    } else {
        match T::try_from(u) {
            Ok(v) => Ok(Some(v)),
            Err(e) => Err(de::Error::custom(format!("{}", e))),
        }
    }
}

pub static OPTIONS: Lazy<Opt> = Lazy::new(Opt::parse);

pub static CONFIG: Lazy<Config> =
    Lazy::new(
        || match awconf::load_config::<Config>("aw-man", &OPTIONS.awconf) {
            Ok(conf) => conf,
            Err(awconf::Error::Deserialization(e)) => {
                error!("{}", e);
                panic!("{}", e);
            }
            Err(e) => {
                panic!("{:#?}", e)
            }
        },
    );

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
        print_formats();
        return false;
    }

    Lazy::force(&FILE_NAME);

    if CONFIG.use_sofware_renderer {
        std::env::set_var("GSK_RENDERER", "cairo");
    }

    true
}
