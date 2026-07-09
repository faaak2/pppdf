use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        // new_manifest() defaults already include PerMonitorV2 DPI awareness,
        // common-controls v6, UTF-8 codepage, and longPathAware.
        embed_manifest(new_manifest("Pppdf.Viewer")).expect("unable to embed manifest");

        // Icon resource only — winresource does NOT embed a manifest unless
        // set_manifest()/set_manifest_file() is called, so this can't collide
        // with the embed-manifest call above (two manifests = linker error).
        let mut res = winresource::WindowsResource::new();
        res.set_icon_with_id("assets/icon.ico", "1");
        res.compile().expect("embed icon resource");
        println!("cargo:rerun-if-changed=assets/icon.ico");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
