fn main() {
    // whisper.cpp's Metal backend uses `@available(...)`, which compiles to a
    // call into clang's runtime (`__isPlatformVersionAtLeast`). Rust links with
    // -nodefaultlibs, so pull that runtime in explicitly for Metal builds.
    let is_macos = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    if is_macos && std::env::var_os("CARGO_FEATURE_METAL").is_some() {
        let out = std::process::Command::new("clang").arg("--print-resource-dir").output();
        if let Ok(out) = out {
            let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !dir.is_empty() {
                println!("cargo:rustc-link-search=native={dir}/lib/darwin");
                println!("cargo:rustc-link-lib=static=clang_rt.osx");
            }
        }
    }
}
