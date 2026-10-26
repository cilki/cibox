use crate::config::android::is_safe_path_component;
use cargo_toml::Manifest;
use std::fs;
use std::path::{Path, PathBuf};

/// How deep below the project root the walk looks for a Gradle root.
const MAX_DEPTH: usize = 3;

/// Upper bound on directories visited, so detection stays fast even in a
/// pathologically wide tree
const MAX_DIRS: usize = 512;

/// How wide a level of the walk has to be before it is read on more than one
/// thread.
const PARALLEL_LEVEL_FROM: usize = 48;

/// Upper bound on the threads a level is spread over. The walk waits on the
/// kernel rather than on a CPU, but every thread still has to be started,
/// and past a handful that costs more than the listings they take over.
const MAX_THREADS: usize = 4;

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

/// One directory of the walk: where it is, and its path relative to the
/// project root
type Visit = (PathBuf, String);

/// The Gradle root of the repository, if there is one within reach.
///
/// The walk is breadth-first, a level at a time, so the shallowest Gradle
/// root wins. Every repository pays for it — a project with no Gradle
/// anywhere is only known to have none once the walk has come up empty — so
/// a level costs one `read_dir` per directory and nothing else, and a wide
/// one is read on several threads.
pub(super) fn gather(path: &Path) -> Option<AndroidFacts> {
    let mut frontier: Vec<Visit> = vec![(path.to_path_buf(), String::new())];
    let mut budget = MAX_DIRS;

    for depth in 0..=MAX_DEPTH {
        if frontier.is_empty() {
            return None;
        }
        // Visiting all of this level would take the walk past the bound, so
        // it gives up after the part that fits — exactly the prefix a queue
        // would have reached before the bound stopped it
        let exhausted = frontier.len() >= budget;
        frontier.truncate(budget);
        budget -= frontier.len();

        // The deepest level has no children to collect, so the two markers
        // are all it is asked for
        let listings = list_level(&frontier, depth < MAX_DEPTH);
        for ((dir, rel), listing) in frontier.iter().zip(&listings) {
            if listing.gradle_root {
                if let Some(facts) = gather_at(dir, rel) {
                    return Some(facts);
                }
            }
        }
        if exhausted {
            return None;
        }
        frontier = listings
            .into_iter()
            .flat_map(|listing| listing.children)
            .collect();
    }
    None
}

/// Read every directory of one level of the walk, in frontier order.
///
/// The walk is almost all kernel time — a `read_dir` is a handful of
/// syscalls returning next to no data — so a wide level goes faster on
/// several threads than on one. A narrow one doesn't: starting a thread
/// costs more than listing a few directories, and the usual repository has
/// a few.
fn list_level(frontier: &[Visit], descend: bool) -> Vec<Listing> {
    let read = |(dir, rel): &Visit| Listing::read(dir, rel, descend);
    // Asking how many threads are available is itself worth avoiding: on
    // Linux it reads the process's cgroup limits, which costs more than the
    // whole walk does in a repository of a dozen directories
    if frontier.len() < PARALLEL_LEVEL_FROM {
        return frontier.iter().map(read).collect();
    }
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(MAX_THREADS);
    if threads < 2 {
        return frontier.iter().map(read).collect();
    }
    std::thread::scope(|scope| {
        let handles: Vec<_> = frontier
            .chunks(frontier.len().div_ceil(threads))
            .map(|chunk| scope.spawn(move || chunk.iter().map(read).collect::<Vec<Listing>>()))
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| {
                // A panic in the walk is cibox's bug to report, with its own
                // backtrace, rather than a level of missing facts
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    })
}

/// What one `read_dir` of a visited directory tells the walk: whether the
/// directory is a Gradle root, and which of its children the walk descends
/// into.
///
/// Both answers come out of the same listing. Asking the filesystem for the
/// Gradle markers separately — `gradlew`, then `settings.gradle`, then
/// `settings.gradle.kts` — costs a `stat` per marker per directory visited,
/// and the walk visits up to [`MAX_DIRS`] of them in a repository that has
/// no Gradle project at all.
#[derive(Default)]
struct Listing {
    gradle_root: bool,
    children: Vec<Visit>,
}

impl Listing {
    /// Read `dir`, whose path relative to the project root is `rel`. With
    /// `descend` false only the Gradle markers are wanted (the walk is at
    /// its depth limit, so the children are of no interest) and the two
    /// `stat`s that answer that are cheaper than listing the directory.
    fn read(dir: &Path, rel: &str, descend: bool) -> Self {
        if !descend {
            return Self {
                gradle_root: is_gradle_root(dir),
                children: Vec::new(),
            };
        }
        let Ok(entries) = fs::read_dir(dir) else {
            // A directory that can't be listed may still be one its contents
            // can be reached through, and that is all the markers need
            return Self {
                gradle_root: is_gradle_root(dir),
                children: Vec::new(),
            };
        };
        let mut listing = Self::default();
        let mut has_launcher = false;
        let mut has_settings = false;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                // Skip build output and dependency trees, and anything whose
                // name could never be safely interpolated into a shell command
                // (which also covers hidden directories)
                if matches!(name, "target" | "node_modules" | "build")
                    || !is_safe_path_component(name)
                {
                    continue;
                }
                let child_rel = if rel.is_empty() {
                    name.to_string()
                } else {
                    format!("{rel}/{name}")
                };
                listing.children.push((entry.path(), child_rel));
                continue;
            }
            // A symlink's own type says nothing about its target, so the
            // markers fall back to the stat `Path::is_file` would have done
            let is_file =
                || file_type.is_file() || (file_type.is_symlink() && entry.path().is_file());
            match name {
                "gradlew" => has_launcher |= is_file(),
                "settings.gradle" | "settings.gradle.kts" => has_settings |= is_file(),
                _ => {}
            }
        }
        listing.gradle_root = has_launcher && has_settings;
        listing
    }
}

/// Whether `dir` holds the two files every Gradle root has
fn is_gradle_root(dir: &Path) -> bool {
    dir.join("gradlew").is_file()
        && (dir.join("settings.gradle").is_file() || dir.join("settings.gradle.kts").is_file())
}

/// The facts of `dir` (at `rel` below the project root), a Gradle root with
/// an Android application module
fn gather_at(dir: &Path, rel: &str) -> Option<AndroidFacts> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{tempdir, TempDir};

    /// A Gradle root at `rel` below `root`: the launcher, the settings file,
    /// and an `app` module with a manifest — the three things detection
    /// looks for
    fn gradle_root(root: &Path, rel: &str) {
        let dir = root.join(rel);
        fs::create_dir_all(dir.join("app/src/main")).unwrap();
        fs::write(dir.join("gradlew"), "#!/bin/sh\n").unwrap();
        fs::write(dir.join("settings.gradle"), "rootProject.name = 'app'\n").unwrap();
        fs::write(dir.join("app/src/main/AndroidManifest.xml"), "<manifest/>").unwrap();
    }

    /// `count` empty directories directly below `root`, named so that none of
    /// them is skipped by the walk
    fn wide(root: &Path, count: usize) {
        for i in 0..count {
            fs::create_dir_all(root.join(format!("pkg{i}"))).unwrap();
        }
    }

    fn project() -> TempDir {
        tempdir().unwrap()
    }

    #[test]
    fn test_no_gradle_project() {
        let dir = project();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        assert!(gather(dir.path()).is_none());
    }

    #[test]
    fn test_gradle_root_at_the_project_root() {
        let dir = project();
        gradle_root(dir.path(), ".");
        let facts = gather(dir.path()).unwrap();
        assert_eq!(facts.dir, ".");
        assert_eq!(facts.app_module, "app");
        // Only a nested root can have a crate above it
        assert_eq!(facts.rust_dir, None);
    }

    #[test]
    fn test_nested_gradle_root_keeps_its_path() {
        let dir = project();
        gradle_root(dir.path(), "a/b/android");
        let facts = gather(dir.path()).unwrap();
        assert_eq!(facts.dir, "a/b/android");
    }

    /// The walk is breadth-first, so the root nearest the project root wins
    /// however the directories are ordered on disk
    #[test]
    fn test_shallowest_gradle_root_wins() {
        let dir = project();
        gradle_root(dir.path(), "zzz/deeper/android");
        gradle_root(dir.path(), "aaa");
        assert_eq!(gather(dir.path()).unwrap().dir, "aaa");
    }

    #[test]
    fn test_gradle_root_past_the_depth_limit_is_not_found() {
        let dir = project();
        gradle_root(dir.path(), "a/b/c/android");
        assert!(gather(dir.path()).is_none());
    }

    #[test]
    fn test_build_output_and_dependency_trees_are_not_searched() {
        for skipped in ["build", "target", "node_modules", ".hidden"] {
            let dir = project();
            gradle_root(dir.path(), &format!("{skipped}/android"));
            assert!(
                gather(dir.path()).is_none(),
                "a Gradle root under {skipped} was taken for the project's"
            );
        }
    }

    /// A directory name that couldn't be interpolated into a shell command is
    /// never entered, so nothing unquotable reaches the generated pipeline
    #[test]
    fn test_unsafe_directory_name_is_not_entered() {
        let dir = project();
        gradle_root(dir.path(), "my project");
        assert!(gather(dir.path()).is_none());
    }

    #[test]
    fn test_gradlew_without_settings_is_not_a_gradle_root() {
        let dir = project();
        gradle_root(dir.path(), "android");
        fs::remove_file(dir.path().join("android/settings.gradle")).unwrap();
        assert!(gather(dir.path()).is_none());
    }

    #[test]
    fn test_settings_without_gradlew_is_not_a_gradle_root() {
        let dir = project();
        gradle_root(dir.path(), "android");
        fs::remove_file(dir.path().join("android/gradlew")).unwrap();
        assert!(gather(dir.path()).is_none());
    }

    #[test]
    fn test_kotlin_settings_file_counts() {
        let dir = project();
        gradle_root(dir.path(), "android");
        let settings = dir.path().join("android/settings.gradle");
        fs::rename(&settings, settings.with_extension("gradle.kts")).unwrap();
        assert_eq!(gather(dir.path()).unwrap().dir, "android");
    }

    /// The markers are read off the directory listing, where a symlink's own
    /// type says nothing about what it points at
    #[test]
    #[cfg(unix)]
    fn test_symlinked_gradlew_counts() {
        let dir = project();
        gradle_root(dir.path(), "android");
        let gradlew = dir.path().join("android/gradlew");
        fs::rename(&gradlew, dir.path().join("android/gradlew.real")).unwrap();
        std::os::unix::fs::symlink("gradlew.real", &gradlew).unwrap();
        assert_eq!(gather(dir.path()).unwrap().dir, "android");
    }

    #[test]
    fn test_module_other_than_app_is_found() {
        let dir = project();
        gradle_root(dir.path(), "android");
        fs::rename(
            dir.path().join("android/app"),
            dir.path().join("android/mobile"),
        )
        .unwrap();
        let facts = gather(dir.path()).unwrap();
        assert_eq!(facts.app_module, "mobile");
    }

    /// A Gradle root with no module holding a manifest is not an Android
    /// project, and the walk goes on looking for one that is
    #[test]
    fn test_gradle_root_without_a_manifest_is_passed_over() {
        let dir = project();
        gradle_root(dir.path(), "aaa");
        fs::remove_file(dir.path().join("aaa/app/src/main/AndroidManifest.xml")).unwrap();
        gradle_root(dir.path(), "bbb");
        assert_eq!(gather(dir.path()).unwrap().dir, "bbb");
    }

    #[test]
    fn test_rust_wrapper_layout_names_the_crate_directory() {
        let dir = project();
        gradle_root(dir.path(), "android");
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[lib]\ncrate-type = [\"cdylib\"]\n",
        )
        .unwrap();
        assert_eq!(gather(dir.path()).unwrap().rust_dir.as_deref(), Some("."));
    }

    #[test]
    fn test_a_plain_crate_above_the_gradle_root_is_not_a_cargo_ndk_build() {
        let dir = project();
        gradle_root(dir.path(), "android");
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        assert_eq!(gather(dir.path()).unwrap().rust_dir, None);
    }

    #[test]
    fn test_min_sdk_comes_from_the_app_module() {
        let dir = project();
        gradle_root(dir.path(), "android");
        fs::write(
            dir.path().join("android/app/build.gradle"),
            "android {\n    minSdk = 26\n}\n",
        )
        .unwrap();
        assert_eq!(gather(dir.path()).unwrap().min_sdk, Some(26));
    }

    #[test]
    fn test_min_sdk_that_is_not_a_bare_number_is_absent() {
        let dir = project();
        gradle_root(dir.path(), "android");
        fs::write(
            dir.path().join("android/app/build.gradle"),
            "android {\n    minSdk = libs.versions.minSdk.get()\n}\n",
        )
        .unwrap();
        assert_eq!(gather(dir.path()).unwrap().min_sdk, None);
    }

    /// A level wide enough to be read on several threads is still read whole,
    /// and in order
    #[test]
    fn test_wide_level_is_read_in_full() {
        let dir = project();
        wide(dir.path(), PARALLEL_LEVEL_FROM * 2);
        let last = format!("pkg{}/android", PARALLEL_LEVEL_FROM * 2 - 1);
        gradle_root(dir.path(), &last);
        assert_eq!(gather(dir.path()).unwrap().dir, last);
    }

    /// Past the bound the walk gives up rather than reading an arbitrarily
    /// wide tree, so a root the first [`MAX_DIRS`] directories don't reach is
    /// not found
    #[test]
    fn test_gradle_root_past_the_directory_bound_is_not_found() {
        let dir = project();
        wide(dir.path(), MAX_DIRS + 8);
        gradle_root(dir.path(), "pkg0/android");
        assert!(gather(dir.path()).is_none());
    }
}
