<p align="center">
	<img src="https://raw.githubusercontent.com/fossable/fossable/master/emblems/cibox.svg" style="width:90%; height:auto;"/>
</p>

![License](https://img.shields.io/github/license/fossable/cibox)
![Build](https://github.com/fossable/cibox/actions/workflows/test.yml/badge.svg)
![GitHub repo size](https://img.shields.io/github/repo-size/fossable/cibox)
![Stars](https://img.shields.io/github/stars/fossable/cibox?style=social)

<hr>

**cibox** is a tool that generates CI/CD configurations for popular platforms
like Github Actions and Gitlab CI. Imagine Terraform, but for CI pipelines.

There are three main advantages to generating your CI workflows/pipelines:

- You can get started really quickly for projects in popular ecosystems.
- You're not locked into a single CI platform since you can easily generate
  pipelines for any platform.
- You don't have to write any Yaml because we have a TUI interface.

The downside is, of course, you don't get the full flexiblity of writing your
own pipeline from scratch. You can use a cibox pipeline as a starting point and
make your own customizations, but then you're not able to freely switch to
another CI platform.

## Supported CI platforms

- Github Actions
- Gitlab
- Circle CI
- Gitea

## How it works

cibox is built around **rules**: small, opinionated units of CI configuration
like `rust-test`, `rust-release`, or `docker-build`. Each rule is triggered by
detection — a `Cargo.toml` enables most of the Rust rules, a `Dockerfile` the
Docker rules, a publishable package enables its release rule — and contributes
jobs to the generated pipeline. Release rules run only on `v*` git tags (e.g.
`rust-release` runs `cargo publish`, `aur-release` pushes to the AUR). Build
rules compile in release mode on every push and upload what they produce as CI
artifacts: `rust-build` collects the `cargo build --release` binaries,
`go-build` the `go build` binaries, `zig-build` the `zig build --release=safe`
install outputs, and `android-build` the release APK.

Running `cibox update` in a project with no configuration at all produces a
working pipeline: detection picks the rules and the target platform is inferred
from existing CI configuration, then from your git remote's host, falling back
to GitHub Actions. After that, a plain `cibox update` refreshes every platform
that has a cibox-managed file (listed below) on disk, and creates nothing for
the others. Pass `--github`, `--gitea`, `--gitlab` or `--circleci` to choose the
platforms explicitly, in any combination.

```
$ cibox                             # interactive TUI to toggle rules
$ cibox detect                      # show project facts and which rules fired
$ cibox validate                    # check cibox.ron and print the resolved rules
$ cibox update                      # write or refresh the pipeline files
$ cibox update --gitlab             # generate for a specific platform
$ cibox update --github --gitlab    # ...or for several at once
```

The files cibox manages, per platform:

| Platform       | Files                                                       |
| -------------- | ----------------------------------------------------------- |
| GitHub Actions | `.github/workflows/ci.yml`, `.github/workflows/release.yml` |
| Gitea Actions  | `.gitea/workflows/ci.yml`, `.gitea/workflows/release.yml`   |
| GitLab CI      | `.gitlab-ci.yml`                                            |
| CircleCI       | `.circleci/config.yml`                                      |

`cibox update` is safe to re-run after you customize the generated files.
Merging happens on a lossless YAML tree, so your comments, formatting, and key
order survive. It conforms the jobs cibox manages to its current output, re-adds
managed jobs that are missing, removes managed jobs whose rule you disabled, and
leaves everything else alone: jobs you added are kept untouched. Pass `--force`
to rewrite the files from scratch instead.

To keep a managed job out of the pipeline without losing it, comment its block
out. `update` leaves the block commented but keeps regenerating its content
(reported as "disabled"), so uncommenting it later gives you a current job, not
a stale one. To drop a job permanently, disable its rule in `cibox.ron` — that
removes it, commented or not.

Pass `--rule <RULE>` (repeatable) to touch only the named rules' jobs; every
other job, cibox-managed or not, is left exactly as it is:

```
$ cibox update --rule rust-test --rule rust-fmt
```

Turning every rule off (or deleting whatever the last rule detected on) prunes
all of cibox's jobs, so `cibox update` is also how you hand a pipeline back to
yourself. A file that would be left with no jobs at all is reported and left
untouched, since every platform rejects an empty pipeline — delete it yourself
if you no longer want it.

### What detection looks for

Detection runs per rule, not per ecosystem. For most rules the ecosystem's
marker file is the whole of it: a `Cargo.toml` enables `rust-test`,
`rust-fmt`, `rust-clippy`, `rust-audit` and `rust-doc`, a `go.mod` all four
Go rules, a `build.zig` all three Zig rules, a `pyproject.toml`, `setup.py`
or `requirements.txt` the three Python checks, a `CMakeLists.txt`
`cmake-build`, and a `Dockerfile` both docker rules. The rest ask for more
than the marker, because with the marker alone they would generate a job
with nothing to run:

- `rust-msrv` needs a `rust-version` in `Cargo.toml` — that declaration is
  what it checks the build against.
- `rust-release` and `python-release` need a publishable package: a
  `[package]` that isn't `publish = false`, or a `pyproject.toml` with a
  named `[project]`. A workspace with no root package has neither, and
  publishing its members in dependency order is not something the rule
  attempts.
- `rust-feature-combos` needs a library crate (`[lib]` or `src/lib.rs`) with a
  `[features]` table, and `rust-minimal-versions` a library that is also
  publishable. Both check what downstream crates get; a binary picks its own
  features and ships its own lockfile.
- `rust-build` needs the opposite — a crate that is *not* a library, since
  the artifacts it uploads are the binaries `cargo build --release` produces.
- `android-build` needs a Gradle root: a `gradlew` beside a `settings.gradle`
  or `settings.gradle.kts`, holding a module with
  `src/main/AndroidManifest.xml`. It is looked for up to three directories
  below the project root, so an app nested in a larger repository is found
  too.
- `node-test` and `node-lint` need a `test` or `lint` script in
  `package.json`, since running that script is the whole job.
- `node-typecheck` and `node-fmt` need `typescript` or `prettier` among the
  project's dependencies as well as the config file. Both tools are run from
  `node_modules/.bin` and never downloaded, so a `tsconfig.json` committed
  just for an editor doesn't enable a job that would have no compiler.
- `cmake-test` needs `enable_testing()` or `include(CTest)` in the root
  `CMakeLists.txt`; `cmake-fmt` needs a `.clang-format`.
- `gitleaks` and `aur-release` are never detected. Scanning the full git
  history for secrets is a choice about a project rather than a fact about
  it, and an AUR package's `PKGBUILD` lives in the AUR repository rather than
  in yours — so both rules are there to be asked for.

`cibox detect` prints the verdict for every rule, so the ones that didn't
fire are listed too — that is where to look for a rule you expected and
didn't get. Any of them can be switched on regardless, and a rule forced on
without its facts still generates a working job: `rust-msrv` reads the
version out of `Cargo.toml` in shell when detection didn't capture it.

```ron
(
    gitleaks: (enabled: true),
    rust_feature_combos: (enabled: true),
)
```

### Token permissions

GitHub and Gitea workflows are generated with `permissions: contents: read`, so
the ambient CI token can check out the code and nothing else — without that key
the token inherits the repository or organization default, which can be
write-all. Jobs that genuinely need more ask for it individually:
`docker-release` adds `packages: write` when the image lives on `ghcr.io`.
`cibox update` only scaffolds the block into a workflow that has none, so if you
widen the token yourself your version is kept.

The generated checkout also passes `persist-credentials: false`. By default
`actions/checkout` writes the token into `.git/config` as an auth header, where
every later step can read it — including build scripts and test suites running
third-party code. Nothing cibox generates uses git after the clone, so the
credential is dropped.

### Where credentials come from

On GitHub and Gitea, a credential is read from repository Secrets and only from
there: `DOCKER_PASSWORD`, `CARGO_REGISTRY_TOKEN`, `TWINE_PASSWORD` and
`AUR_SSH_PRIVATE_KEY` lower to `NAME: ${{ secrets.NAME }}`. A Variable is not a
quieter Secret — its value is displayed in the settings UI, handed back in
plaintext by the variables API and never masked in a log — so cibox offers no
`|| vars.NAME` fallback for a credential, which would amount to saying that
keeping a publish token there is fine. Values that aren't sensitive do get the
choice: `DOCKER_USERNAME` lowers to
`${{ secrets.DOCKER_USERNAME || vars.DOCKER_USERNAME }}` and can live in either
store. GitLab and CircleCI read both kinds from project CI variables, which the
`# Required CI variables:` header at the top of the generated file lists for
you.

## `cibox.ron`

`cibox.ron` holds only your _overrides_ from the detected defaults — no file is
needed until you want to change something. Each rule has an `enabled` switch
(like NixOS services) plus a few knobs where they make sense:

```ron
(
    // Force a rule off that detection enabled
    rust_audit: (enabled: false),

    // Force a rule on that detection missed
    go_lint: (enabled: true),

    // Set a knob without touching enablement
    docker_build: (image_name: "example/app"),
)
```

Omitted rules follow detection; an omitted `enabled` leaves detection in charge
while still applying the knobs. The target platform is not part of the file —
`cibox update` can generate for any platform, so pass `--github`, `--gitlab`,
etc., or let cibox use the pipelines already on disk.

`image_name` has to be a valid docker reference, because it ends up as a literal
argument to `docker build -t`: lowercase alphanumerics separated by `.`, `-` or
`_`, optionally prefixed with a registry host and suffixed with a `:tag`.
Anything else is rejected by `cibox validate`. The default is derived from the
git remote (or the directory name) and normalized to fit, so a `Fossable/CiBox`
remote becomes `fossable/cibox`.

The registry is inferred from the image name. `docker-release` logs in to
`ghcr.io` with `GITHUB_TOKEN`, and everywhere else — the host prefix if the name
has one, Docker Hub otherwise — with the `DOCKER_USERNAME` and `DOCKER_PASSWORD`
secrets. If the credentials aren't configured in CI, the login is skipped, for
registries that don't require any.

`sync_readme` additionally pushes `README.md` to the Docker Hub repository
description once the image is released, reusing the same two secrets. It is
Docker Hub only — `ghcr.io` has no description API — and like the login it is
skipped when the credentials aren't configured or there is no `README.md`.

### Multi-arch docker images

`docker-release` builds a single image for the runner's own architecture by
default. Listing `platforms` turns it into a multi-arch build:

```ron
(
    docker_release: (
        image_name: "ghcr.io/example/app",
        platforms: [LinuxAmd64, LinuxArm64],
    ),
)
```

The available platforms are `LinuxAmd64`, `LinuxArm64`, `LinuxArmV7`,
`LinuxRiscv64`, and `WindowsAmd64`. Linux targets build together in one
`docker buildx` job, with QEMU set up when a foreign architecture is involved.
`WindowsAmd64` can't be emulated, so it gets its own job on a Windows runner —
that means GitHub Actions or Gitea Actions only (GitLab and CircleCI refuse to
generate), and the same Dockerfile has to work from a Windows base image. Mixing
Linux and Windows targets emits three jobs: the two per-OS builds push staging
tags and a third merges them into one multi-platform manifest, so `image_name`
must not already carry a tag.

### Publishing to the AUR

`aur-release` publishes packages to the [AUR](https://aur.archlinux.org) when a
version tag is pushed. It is off by default — the AUR repo, not your project,
holds the PKGBUILD, so there is nothing to detect — and enabled in `cibox.ron`:

```ron
(
    aur_release: (enabled: true),

    // Or, for more than one package per repository:
    // aur_release: (enabled: true, packages: ["app", "app-extras"]),
)
```

`packages` defaults to a single package named after the git remote's repository.
Each name becomes one release job that clones
`ssh://aur@aur.archlinux.org/<name>.git`, sets `pkgver` from the tag (with the
leading `v` stripped) and resets `pkgrel`, refreshes the checksums with
`updpkgsums`, test-builds the package with `makepkg` and lints it with `namcap`,
regenerates `.SRCINFO`, and pushes. A broken PKGBUILD therefore fails the job
instead of reaching the AUR. Because `updpkgsums` downloads the sources, the
PKGBUILD's `source` URL has to be live when the tag is pushed — tag-derived
archive URLs (codeload, GitLab archives) are, manually-uploaded release assets
may not be yet.

Names must be valid AUR package names — lowercase alphanumerics and `@._+-`, not
starting with `-` or `.` — which `cibox validate` checks.

The job authenticates with the `AUR_SSH_PRIVATE_KEY` secret. Generate a
dedicated key with `ssh-keygen -t ed25519`, add the public half to your AUR
account, and store the private half as a Secret (GitHub/Gitea) or a project CI
variable (GitLab/CircleCI). On GitLab use type Variable with variable expansion
turned off — the value is multi-line, which rules out masking and type File
would hand the job a path instead of the key.

You can use our TUI interface to edit this file or any editor with LSP support.
Configure your editor to use `cibox lsp` as an LSP and you'll get inline
documenation and autocomplete.

### Editor setup

For [Helix](https://helix-editor.com), add this to your `languages.toml`:

```toml
[language-server]
cibox-lsp = { command = "cibox", args = ["lsp"] }

[[language]]
name = "ron"
file-types = ["ron", { glob = "cibox.ron" }]
language-servers = ["cibox-lsp"]
```
