use crate::config::android::is_safe_path_component;
use cargo_toml::Manifest;
use std::collections::VecDeque;
use std::fs;
use std::path::Path;

/// How deep below the project root the walk looks for a Gradle root.
const MAX_DEPTH: usize = 3;

/// Upper bound on directories visited, so detection stays fast even in a
/// pathologically wide tree
const MAX_DIRS: usize = 512;

/// Facts about a Gradle Android project, possibly nested in the repository
#[derive(Debug, Clone, Default)]
pub struct AndroidFacts {
    /// The Gradle root (the directory holding `gradlew`), relative to the
    /// project root; `.` for the root itself. Shell-safe by construction:
    /// the walk never enters a directory whose name could not be
    /// interpolated into a command, so a project in an unsafe path simply
    /// yields no facts.
    pub dir: String,
    /// The module containing `src/main/AndroidManifest.xml`, relative to
    /// `dir`; usually `app`
    pub app_module: String,
    /// The directory of a `Cargo.toml` declaring a cdylib crate adjacent to
    /// `dir` (its parent), relative to the project root — the signal that a
    /// cargo-ndk pre-build must produce the JNI libraries before Gradle runs
    pub rust_dir: Option<String>,
    /// `minSdk` parsed from the app module's build.gradle(.kts), which
    /// becomes cargo-ndk's `--platform`
    pub min_sdk: Option<u32>,
}

pub(super) fn gather(path: &Path) -> Option<AndroidFacts> {
    // Breadth-first, so the shallowest Gradle root wins
    let mut queue: VecDeque<(std::path::PathBuf, String, usize)> =
        VecDeque::from([(path.to_path_buf(), String::new(), 0)]);
    let mut visited = 0;

    while let Some((dir, rel, depth)) = queue.pop_front() {
        visited += 1;
        if visited > MAX_DIRS {
            return None;
        }

        if let Some(facts) = gather_at(&dir, &rel) {
            return Some(facts);
        }

        if depth >= MAX_DEPTH {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            // Skip build output and dependency trees, and anything whose
            // name could never be safely interpolated into a shell command
            // (which also covers hidden directories)
            if matches!(name, "target" | "node_modules" | "build") || !is_safe_path_component(name)
            {
                continue;
            }
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let child_rel = if rel.is_empty() {
                name.to_string()
            } else {
                format!("{rel}/{name}")
            };
            queue.push_back((entry.path(), child_rel, depth + 1));
        }
    }
    None
}

/// The facts if `dir` (at `rel` below the project root) is a Gradle root
/// with an Android application module
fn gather_at(dir: &Path, rel: &str) -> Option<AndroidFacts> {
    if !dir.join("gradlew").is_file()
        || !(dir.join("settings.gradle").is_file() || dir.join("settings.gradle.kts").is_file())
    {
        return None;
    }
    let app_module = find_app_module(dir)?;

    Some(AndroidFacts {
        dir: if rel.is_empty() {
            ".".into()
        } else {
            rel.into()
        },
        min_sdk: min_sdk(&dir.join(&app_module)),
        rust_dir: rust_dir(dir, rel),
        app_module,
    })
}

/// The module directory containing an Android manifest. `app` is the
/// conventional name, so it wins over whatever else a multi-module project
/// holds; beyond that, directory iteration order decides.
fn find_app_module(dir: &Path) -> Option<String> {
    let is_module = |name: &str| {
        dir.join(name)
            .join("src/main/AndroidManifest.xml")
            .is_file()
    };
    if is_module("app") {
        return Some("app".to_string());
    }
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|name| is_safe_path_component(name))
        .find(|name| is_module(name))
}

/// `minSdk` from the module's build.gradle(.kts). The value becomes
/// cargo-ndk's `--platform` in a generated `run:` step, so only a bare
/// number is accepted — anything else means the fact is better off absent
/// and cargo-ndk falls back to its own default.
fn min_sdk(module: &Path) -> Option<u32> {
    let contents = fs::read_to_string(module.join("build.gradle"))
        .or_else(|_| fs::read_to_string(module.join("build.gradle.kts")))
        .ok()?;
    contents.lines().find_map(|line| {
        let rest = line
            .trim_start()
            .strip_prefix("minSdkVersion")
            .or_else(|| line.trim_start().strip_prefix("minSdk"))?;
        let value = rest.trim_start().trim_start_matches('=').trim();
        value
            .parse::<u32>()
            .ok()
            .filter(|sdk| (1..=99).contains(sdk))
    })
}

/// The parent of the Gradle root, if it holds a cdylib crate — the layout of
/// a Rust app wrapped in a Gradle shell (`<crate>/android`, or `android` at
/// the root of the crate itself)
fn rust_dir(dir: &Path, rel: &str) -> Option<String> {
    // A Gradle root at the project root has no parent inside the project
    if rel.is_empty() {
        return None;
    }
    let parent_rel = match rel.rsplit_once('/') {
        Some((parent, _)) => parent.to_string(),
        None => ".".to_string(),
    };
    let contents = fs::read_to_string(dir.parent()?.join("Cargo.toml")).ok()?;
    let manifest = Manifest::from_str(&contents).ok()?;
    manifest
        .lib
        .as_ref()
        .is_some_and(|lib| lib.crate_type.iter().any(|t| t == "cdylib"))
        .then_some(parent_rel)
}
