use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        // new_manifest() defaults already include PerMonitorV2 DPI awareness,
        // common-controls v6, UTF-8 codepage, and longPathAware.
        embed_manifest(new_manifest("Pppdf.Viewer")).expect("unable to embed manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
