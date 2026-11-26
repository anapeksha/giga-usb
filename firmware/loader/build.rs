use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=loader.x");
    if env::var("TARGET").as_deref() != Ok("thumbv7em-none-eabihf") {
        return;
    }

    let script = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("loader.x");
    println!("cargo:rustc-link-arg-bin=loader=-T{}", script.display());
    println!("cargo:rustc-link-arg-bin=loader=--build-id=none");
}
