use super::Rule;
use crate::config::android::{is_safe_path_component, is_valid_project_dir, is_valid_toolchain};
use crate::detection::ProjectFacts;
use crate::ir::{Job, Stage, Step};

/// Pinned so the job neither breaks when Google rotates "latest" nor
/// silently changes toolchains; the checksum is of this exact artifact
const CMDLINE_TOOLS_URL: &str =
    "https://dl.google.com/android/repository/commandlinetools-linux-15859902_latest.zip";
const CMDLINE_TOOLS_SHA256: &str =
    "4e4c464f145a7512b57d088ac6c278c03c9eea610886b35a5e0804e74eedf583";

/// Only cargo-ndk needs the NDK spelled out — AGP downloads its own
const NDK_VERSION: &str = "26.1.10909125";

/// The one ABI built for now: every Android device released this decade is
/// arm64, and the APK is a build canary rather than a shipped artifact
const ABI: &str = "arm64-v8a";
const ABI_TRIPLE: &str = "aarch64-linux-android";

/// The Android SDK lives inside the checkout so one project-relative cache
/// path works on every platform — the same argument that moves CARGO_HOME in
/// the rust rules. It must be exported as an absolute path per step (gradle
/// and cargo-ndk both `cd` first, which would re-anchor a relative one).
const SDK_DIR: &str = ".android-sdk";
const GRADLE_HOME: &str = ".gradle-home";

/// Build the release APK of a Gradle Android project on every push and
/// upload it as a CI artifact. Release mode is deliberate even though
/// nothing is published: F-Droid builds releases from source at version
/// tags, so an unbuildable release configuration should fail CI here, not
/// on F-Droid's buildserver.
///
/// When the Gradle root sits inside a Rust cdylib crate (the Bevy/Tauri
/// mobile layout), a cargo-ndk pre-build produces the JNI libraries first.
pub struct AndroidBuild {
    /// Gradle project directory override; validated by `parse_config`
    /// (interpolated into shell commands)
    pub dir: Option<String>,
    /// Extra Debian packages the native build needs, validated like `dir`
    pub apt_packages: Vec<String>,
    /// Toolchain for the cargo-ndk pre-build: "stable", "nightly", "x.y[.z]"
    pub rust_toolchain: Option<String>,
}

/// The effective project layout: detected facts overlaid with the dir knob,
/// with fallbacks so a force-enabled rule still emits a plausible job
struct Layout {
    dir: String,
    app_module: String,
    rust_dir: Option<String>,
    min_sdk: Option<u32>,
}

impl AndroidBuild {
    fn layout(&self, facts: &ProjectFacts) -> Layout {
        let detected = facts.android.as_ref();
        // Everything here lands in shell commands. Each value was already
        // validated at its source (detection refuses unsafe paths,
        // parse_config rejects malformed knobs), so an invalid one slipping
        // through is a bug — but falling back beats interpolating it.
        let dir = self
            .dir
            .as_deref()
            .filter(|d| is_valid_project_dir(d))
            .or_else(|| {
                detected
                    .map(|a| a.dir.as_str())
                    .filter(|d| is_valid_project_dir(d))
            })
            .unwrap_or(".")
            .to_string();
        let app_module = detected
            .map(|a| a.app_module.as_str())
            .filter(|m| is_safe_path_component(m))
            .unwrap_or("app")
            .to_string();
        Layout {
            dir,
            app_module,
            rust_dir: detected
                .and_then(|a| a.rust_dir.clone())
                .filter(|d| is_valid_project_dir(d)),
            min_sdk: detected.and_then(|a| a.min_sdk),
        }
    }

    /// `cd ./<dir> && ` unless `dir` is the workspace root
    fn enter(dir: &str) -> String {
        if dir == "." {
            String::new()
        } else {
            format!("cd ./{dir} && ")
        }
    }

    fn steps(&self, layout: &Layout) -> Vec<Step> {
        let mut steps = vec![Step::checkout()];

        // CIBOX_IMAGE ships the JDK; the rust images don't, and native
        // builds may need extra host libraries on either
        if layout.rust_dir.is_some() || !self.apt_packages.is_empty() {
            let mut packages = vec!["default-jdk-headless", "unzip"];
            packages.extend(self.apt_packages.iter().map(String::as_str));
            steps.push(Step::run(
                "Install build tools",
                format!(
                    "apt-get update && apt-get install -y --no-install-recommends {}",
                    packages.join(" ")
                ),
            ));
        }

        // cmdline-tools unpack as `cmdline-tools/`, but sdkmanager requires
        // the `cmdline-tools/latest/` layout to agree with its own path.
        // Accepting the licenses up front is what lets AGP download the
        // platform and build-tools the project asks for on its own.
        steps.push(Step::run(
            "Install Android SDK",
            format!(
                "export ANDROID_HOME=\"$PWD/{SDK_DIR}\"; \
                 if [ ! -d \"$ANDROID_HOME/cmdline-tools/latest\" ]; then \
                 curl -fsSL {CMDLINE_TOOLS_URL} -o /tmp/cmdline-tools.zip && \
                 echo \"{CMDLINE_TOOLS_SHA256}  /tmp/cmdline-tools.zip\" | sha256sum -c - && \
                 mkdir -p \"$ANDROID_HOME/cmdline-tools\" && \
                 unzip -q /tmp/cmdline-tools.zip -d \"$ANDROID_HOME/cmdline-tools\" && \
                 mv \"$ANDROID_HOME/cmdline-tools/cmdline-tools\" \"$ANDROID_HOME/cmdline-tools/latest\"; \
                 fi; \
                 yes | \"$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager\" --licenses > /dev/null"
            ),
        ));

        if let Some(rust_dir) = &layout.rust_dir {
            steps.extend(self.rust_steps(layout, rust_dir));
        }

        steps.push(Step::run(
            "Build release APK",
            format!(
                "export ANDROID_HOME=\"$PWD/{SDK_DIR}\" GRADLE_USER_HOME=\"$PWD/{GRADLE_HOME}\"; \
                 {}chmod +x ./gradlew && ./gradlew --no-daemon assembleRelease",
                Self::enter(&layout.dir)
            ),
        ));
        steps
    }

    /// Produce the JNI libraries before Gradle packages them
    fn rust_steps(&self, layout: &Layout, rust_dir: &str) -> Vec<Step> {
        // cargo-ndk writes into the Gradle module relative to the crate it
        // builds from, which is the Gradle root's parent
        let gradle_base = layout.dir.rsplit('/').next().unwrap_or(&layout.dir);
        let jni_dir = format!("{gradle_base}/{}/src/main/jniLibs", layout.app_module);
        let platform = layout
            .min_sdk
            .map(|sdk| format!(" --platform {sdk}"))
            .unwrap_or_default();

        vec![
            // AGP downloads its own NDK, but cargo-ndk won't
            Step::run(
                "Install NDK",
                format!(
                    "export ANDROID_HOME=\"$PWD/{SDK_DIR}\"; \
                     \"$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager\" \"ndk;{NDK_VERSION}\" > /dev/null"
                ),
            ),
            // Lands in .cargo/bin, which cargo searches for subcommands, so
            // a cache hit turns the install into a no-op instead of a rebuild
            Step::run(
                "Install cargo-ndk",
                format!(
                    "export CARGO_HOME=\"$PWD/.cargo\"; \
                     rustup target add {ABI_TRIPLE} && cargo install cargo-ndk"
                ),
            ),
            // CARGO_HOME and the target dir are pinned to the workspace root
            // before the cd, so the cache paths and the subcommand lookup
            // keep pointing at the same directories for any crate layout.
            // --link-libcxx-shared bundles libc++_shared.so, which cdylibs
            // built against the NDK need at load time.
            Step::run(
                "Build native library",
                format!(
                    "export ANDROID_HOME=\"$PWD/{SDK_DIR}\" \
                     ANDROID_NDK_HOME=\"$PWD/{SDK_DIR}/ndk/{NDK_VERSION}\" \
                     CARGO_HOME=\"$PWD/.cargo\" CARGO_TARGET_DIR=\"$PWD/target\"; \
                     {}cargo ndk -t {ABI}{platform} -o ./{jni_dir} build --release --link-libcxx-shared",
                    Self::enter(rust_dir)
                ),
            ),
        ]
    }
}

impl Rule for AndroidBuild {
    fn id(&self) -> &'static str {
        "android-build"
    }

    fn name(&self) -> &'static str {
        "Android APK"
    }

    fn description(&self) -> &'static str {
        "Build the release APK and upload it as a CI artifact"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        facts.android.is_some()
    }

    fn jobs(&self, facts: &ProjectFacts) -> Vec<Job> {
        let layout = self.layout(facts);

        let image = match layout.rust_dir {
            Some(_) => super::rust::toolchain_image(
                self.rust_toolchain
                    .as_deref()
                    .filter(|t| is_valid_toolchain(t))
                    .unwrap_or("stable"),
            ),
            None => super::CIBOX_IMAGE.to_string(),
        };

        let mut cache_paths = vec![format!("{SDK_DIR}/"), format!("{GRADLE_HOME}/")];
        if layout.rust_dir.is_some() {
            cache_paths.extend([".cargo/".to_string(), "target/".to_string()]);
        }

        let artifact_dir = format!(
            "{}{}/build/outputs/apk/release/",
            if layout.dir == "." {
                String::new()
            } else {
                format!("{}/", layout.dir)
            },
            layout.app_module
        );

        vec![Job::new(self.id(), self.name(), Stage::Build)
            .with_image(image)
            // The cold path bootstraps the SDK and cross-compiles; the cache
            // makes the steady state far faster
            .with_timeout(90)
            .with_cache("android-build-cache", cache_paths)
            .with_artifacts(vec![artifact_dir])
            .with_steps(self.steps(&layout))]
    }
}
