// build.rs

use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let out_dir = env::var_os("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("version.rs");
    fs::write(
        &dest_path,
        "
         #[allow(dead_code)]
         #[allow(clippy::all)]
         use clap::crate_version;
         #[doc=r#\"Version string\"#]
         pub const CLAP_LONG_VERSION: &str = crate_version!();
        ",
    )
    .unwrap();
    println!("cargo:rerun-if-changed=build.rs");
}
