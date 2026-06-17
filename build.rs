#![allow(unused_variables)]

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-link-lib=ssl");
    println!("cargo:rustc-link-lib=crypto");
    println!("cargo:rustc-link-lib=z");
    println!("cargo:rustc-link-lib=iconv");
    
    // Ensure the Debian package metadata is correctly set
    println!("cargo:package.metadata.deb.copyright = "© 2026 Tharuk Renuja"");
}