use roniker::RustAnalyzer;
use std::env;
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let mut analyzer = RustAnalyzer::with_root_type("crate::config::ron_types::CiboxConfig");

    analyzer
        .add_file(&root.join("src/config/ron_types.rs"))
        .expect("Failed to parse ron_types.rs");

    analyzer
        .add_file(&root.join("src/config/platform.rs"))
        .expect("Failed to parse platform.rs");

    let dest = PathBuf::from(env::var("OUT_DIR").unwrap()).join("rust_analyzer.json");
    let json = serde_json::to_string(&analyzer).expect("Failed to serialize RustAnalyzer");
    std::fs::write(&dest, json).expect("Failed to write rust_analyzer.json");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/config/ron_types.rs");
    println!("cargo:rerun-if-changed=src/config/platform.rs");
}
