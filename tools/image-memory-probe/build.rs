fn main() {
    let source = std::env::var_os("IMAGE_PROBE_ASSETS_SOURCE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../client/src/image_assets.rs")
        })
        .canonicalize()
        .expect("image asset source");
    println!("cargo:rerun-if-env-changed=IMAGE_PROBE_ASSETS_SOURCE");
    println!("cargo:rerun-if-changed={}", source.display());
    println!(
        "cargo:rustc-env=IMAGE_PROBE_ASSETS_SOURCE={}",
        source.display()
    );
}
