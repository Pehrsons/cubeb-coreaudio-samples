use std::path::PathBuf;
use std::process::Command;

fn main() {
    cc::Build::new()
        .file("src/log.c")
        .compile("cubeb_log_internal");

    // A standalone dylib rather than a linked library: it is meant to be loaded with
    // DYLD_INSERT_LIBRARIES so that it also covers allocations made by the system audio
    // frameworks, not just ours. Built here so it never goes stale against the source.
    println!("cargo:rerun-if-changed=src/zeroing_malloc.c");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("libzeroing_malloc.dylib");
    let status = Command::new("clang")
        .args(["-dynamiclib", "-O2", "-o"])
        .arg(&out)
        .arg("src/zeroing_malloc.c")
        .status();
    match status {
        Ok(status) if status.success() => {
            println!("cargo:rustc-env=ZEROING_MALLOC_DYLIB={}", out.display());
        }
        Ok(status) => {
            println!("cargo:warning=building the zeroing malloc dylib failed: {}", status)
        }
        Err(e) => println!("cargo:warning=could not run clang for the zeroing malloc dylib: {}", e),
    }
}
