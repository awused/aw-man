extern crate metadeps;

fn main() {
    metadeps::probe().unwrap();
    #[cfg(not(windows))]
    pkg_config::Config::new()
        .atleast_version("0.3.7")
        .probe("libjxl")
        .unwrap();
}
