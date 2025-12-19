extern crate metadeps;


#[cfg(target_env = "msvc")]
use embed_resource::CompilationResult;

fn main() {
    metadeps::probe().unwrap();
    #[cfg(target_env = "msvc")]
    {
        static MANIFEST: &str = "windows-manifest.xml";

        let mut manifest = std::env::current_dir().unwrap();
        manifest.push(MANIFEST);

        println!("cargo:rerun-if-changed={}", MANIFEST);
        println!("cargo:rustc-link-arg-bin=aw-man=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bin=aw-man=/MANIFESTINPUT:{}", manifest.to_str().unwrap());
        // Turn linker warnings into errors.
        println!("cargo:rustc-link-arg-bin=aw-man=/WX");

        match embed_resource::compile("resources.rc", embed_resource::NONE) {
            CompilationResult::Ok | CompilationResult::NotWindows => {}
            CompilationResult::NotAttempted(e) | CompilationResult::Failed(e) => panic!("{}", e),
        }
    }
}
