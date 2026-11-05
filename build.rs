//! Compile the C runtime once, at build time. The object file is embedded in
//! the quill binary (see `RUNTIME_OBJECT` in main.rs), so linking a program
//! doesn't recompile it and quill doesn't need its source tree at run time.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=runtime/runtime.c");
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        return; // the playground build: no runtime, nothing gets linked
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("runtime.o");
    let status = Command::new("clang")
        .args(["-O2", "-Wall", "-Wextra", "-c", "runtime/runtime.c", "-o"])
        .arg(&out)
        .status()
        .expect("couldn't run clang (install the Xcode command line tools)");
    assert!(status.success(), "compiling runtime/runtime.c failed");
}
