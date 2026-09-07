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

    // Every file in src/presets except mod.rs defines one preset struct
    let presets_dir = root.join("src/presets");
    let mut preset_files: Vec<PathBuf> = std::fs::read_dir(&presets_dir)
        .expect("Failed to read src/presets")
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            (path.extension().is_some_and(|ext| ext == "rs")
                && path.file_name().is_some_and(|name| name != "mod.rs"))
            .then_some(path)
        })
        .collect();
    preset_files.sort();

    for path in &preset_files {
        analyzer
            .add_file(path)
            .unwrap_or_else(|e| panic!("Failed to parse {}: {e}", path.display()));

        // The preset structs use container-level #[serde(default)] with manual
        // Default impls, which roniker's #[derive(Default)] detection misses —
        // mark them defaulted so the LSP doesn't flag omitted fields as missing.
        let module = path.file_stem().unwrap().to_str().unwrap().to_string();
        let source = std::fs::read_to_string(path).unwrap();
        let struct_name = preset_struct_name(&source)
            .unwrap_or_else(|| panic!("No #[derive(...Preset...)] struct in {}", path.display()));
        let type_path = format!("crate::presets::{module}::{struct_name}");
        let mut info = analyzer
            .get_type_info(&type_path)
            .unwrap_or_else(|| panic!("preset type {type_path} not found"))
            .clone();
        info.has_default = true;
        analyzer.add_type(info);
    }

    let dest = PathBuf::from(env::var("OUT_DIR").unwrap()).join("rust_analyzer.json");
    let json = serde_json::to_string(&analyzer).expect("Failed to serialize RustAnalyzer");
    std::fs::write(&dest, json).expect("Failed to write rust_analyzer.json");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/config/ron_types.rs");
    println!("cargo:rerun-if-changed=src/config/platform.rs");
    println!("cargo:rerun-if-changed=src/presets");
}

/// Find the struct name of the first `pub struct` following a
/// `#[derive(...Preset...)]` attribute
fn preset_struct_name(source: &str) -> Option<String> {
    let mut derive_block = None::<String>;
    let mut deriving_preset = false;
    for line in source.lines() {
        let line = line.trim();
        if line.starts_with("#[derive(") {
            derive_block = Some(line.to_string());
        }
        if let Some(block) = &mut derive_block {
            if !line.starts_with("#[derive(") {
                block.push_str(line);
            }
            if block.ends_with(")]") {
                // Match the derive name, not substrings like "PresetField"
                deriving_preset = block
                    .trim_start_matches("#[derive(")
                    .trim_end_matches(")]")
                    .split(',')
                    .any(|d| d.trim() == "Preset");
                derive_block = None;
            }
        } else if let Some(rest) = line.strip_prefix("pub struct ") {
            if deriving_preset {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                return Some(name);
            }
        } else if !line.starts_with('#') && !line.starts_with("///") {
            deriving_preset = false;
        }
    }
    None
}
