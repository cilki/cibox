`cibox` is a tool that generates CI/CD configurations for multiple platforms
(GitHub Actions, GitLab CI, CircleCI, Gitea) from detected project
facts. It is built around **rules**: opinionated units of CI configuration
(one per concern, e.g. `rust-test`, `rust-release`, `docker-build`) that are
auto-enabled by detection and individually overridable in `cibox.ron`.

## Architecture

- `cibox/src/detection/` gathers [`ProjectFacts`] infallibly — every gatherer
  runs, unreadable files just mean the fact is absent.
- `cibox/src/rules/` holds the `Rule` trait and one struct per rule. A rule
  `detect`s against the facts and emits jobs in the platform-neutral IR
  (`cibox/src/ir.rs`: `Job`, `Step`). Rules are opinionated; only a few carry
  knobs (e.g. the docker image name). `rules::resolve` combines detection
  with `cibox.ron` overrides (`enabled = override.unwrap_or(detected)`).
- Per-platform backends in `cibox/src/platforms/*/lower.rs` lower the IR.
  Steps are shell commands only — platform idioms (checkout actions, caching,
  artifacts, tag gating, triggers) are owned by the backends. Jobs with
  `tags_only` run on `v*` tags everywhere; GitHub/Gitea put them in a
  separate `release.yml` (see `cibox/src/generator/mod.rs`).
- `cibox/src/config/ron_types.rs` defines the `cibox.ron` schema: delta-only
  rule overrides at the top level, nix-services style
  (`rule_name: (enabled: bool, ...knobs)`). The platform is not configured
  there — `config::target_platforms` takes the `--github`/`--gitea`/
  `--gitlab`/`--circleci` flags, else every platform with a cibox-managed
  file on disk (`Platform::managed_paths`), else inference. These types must stay
  hand-written literals — `build.rs` feeds this file to roniker for the RON
  LSP (`cibox lsp`).
- `cibox update` merges into existing files instead of overwriting:
  `generator::plan` lists the managed files and their jobs, and
  `generator::merge_file` rewrites each managed job's own lines in the text
  and takes everything else through a lossless `yaml-edit` syntax tree, so
  user comments and formatting survive — replacing a job on the tree instead
  pulls the blank lines and comment indentation that follow it into the
  value being replaced. Managed jobs (ownership via
  `Rule::owns_job_id`, narrowed by `--rule` through `generator::JobFilter`)
  are conformed, re-added when missing, or pruned when their rule is off;
  everything the user added is preserved. A commented-out managed job stays
  commented but gets its content regenerated ("disabled") —
  `generator::disabled` finds those blocks by uncommenting candidate comment
  runs and parsing them. Missing/empty files and `--force` get the full
  `generator::render_file` output. What happens to each file is decided by
  `generator::pending_update`, which writes nothing.
- The editor TUI (`cibox/src/editor/`) only ever writes `cibox.ron`. Its
  preview pane diffs `generator::pending_update` against the files on disk.
  That is the same decision `cibox update` makes, so the preview can't
  drift from the command. Files update would create show as all-added; the
  pane is hidden only when nothing would change. Every preview line records
  the rule whose job it belongs to (via `Rule::owns_job_id`), so the pane
  scrolls to the selected rule's changed job and marks its lines in the
  gutter.

To add a rule:

1. Add a struct + `impl Rule` in the matching `cibox/src/rules/<env>.rs`
   (or a new file), and register it in `resolve()` in `rules/mod.rs`.
2. Add a snake_case field for it to `CiboxConfig` in
   `cibox/src/config/ron_types.rs` (type `RuleToggle`, or a dedicated struct
   if the rule has knobs) and name the field in the `rule_overrides!` list
   below it — the id-keyed accessors are generated from that one list, and a
   rule's kebab-case id is just its field name. A unit test checks that every
   rule has an entry.
3. Give it a doc comment on the `CiboxConfig` field — roniker surfaces it in
   the LSP.

Jobs that need tools beyond a language toolchain should run on the
`fossable/cibox` image (`CIBOX_IMAGE`), built from the `Dockerfile` at the
repo root — add the tool there. The exception is `aur-release`, which runs on
`archlinux:base-devel` because makepkg only exists on Arch. `android-build`
installs the Android SDK at job runtime into a cached directory inside the
checkout instead — it is multiple GB, far too large for the image — the same
reasoning that relocates CARGO_HOME into the checkout in the rust rules.

## Generated pipeline behavior

This is the user-visible contract of the generated pipelines. The README only
gives a high-level overview, so this section is the reference — keep it in
sync with the code.

### Rules and artifacts

Each rule is triggered by detection — a `Cargo.toml` enables the Rust rules, a
`Dockerfile` enables the Docker rules, a publishable package enables its
release rule — and contributes jobs to the generated pipeline. Release rules
run only on `v*` git tags (e.g. `rust-release` runs `cargo publish`,
`aur-release` pushes to the AUR). Build rules compile in release mode on every
push and upload what they produce as CI artifacts: `rust-build` collects the
`cargo build --release` binaries, `go-build` the `go build` binaries,
`zig-build` the `zig build --release=safe` install outputs, and `android-build`
the release APK.

### Platform selection and managed files

With no configuration at all, detection picks the rules and the target
platform is inferred from existing CI configuration, then from the git
remote's host, falling back to GitHub Actions. After that, a plain
`cibox update` refreshes every platform that has a cibox-managed file on disk,
and creates nothing for the others. `--github`, `--gitea`, `--gitlab` and
`--circleci` choose the platforms explicitly, in any combination. The platform
is never part of `cibox.ron`.

| Platform       | Files                                                       |
| -------------- | ----------------------------------------------------------- |
| GitHub Actions | `.github/workflows/ci.yml`, `.github/workflows/release.yml` |
| Gitea Actions  | `.gitea/workflows/ci.yml`, `.gitea/workflows/release.yml`   |
| GitLab CI      | `.gitlab-ci.yml`                                            |
| CircleCI       | `.circleci/config.yml`                                      |

### Merging on `cibox update`

Merging happens on a lossless YAML tree, so user comments, formatting, and key
order survive. Update conforms the jobs cibox manages to its current output,
re-adds managed jobs that are missing, removes managed jobs whose rule is
disabled, and leaves everything else alone: user-added jobs are kept
untouched. `--force` rewrites the files from scratch instead.

To keep a managed job out of the pipeline without losing it, the user comments
its block out. `update` leaves the block commented but keeps regenerating its
content (reported as "disabled"), so uncommenting it later gives a current
job, not a stale one. Disabling the rule in `cibox.ron` removes the job,
commented or not.

`--rule <RULE>` (repeatable) touches only the named rules' jobs; every other
job, cibox-managed or not, is left exactly as it is:

```
$ cibox update --rule rust-test --rule rust-fmt
```

Turning every rule off (or deleting whatever the last rule detected on) prunes
all of cibox's jobs, so `cibox update` is also how a user hands a pipeline back
to themselves. A file that would be left with no jobs at all is reported and
left untouched, since every platform rejects an empty pipeline — the user
deletes it themselves.

### Token permissions

GitHub and Gitea workflows are generated with `permissions: contents: read`, so
the ambient CI token can check out the code and nothing else — without that key
the token inherits the repository or organization default, which can be
write-all. Jobs that genuinely need more ask for it individually:
`docker-release` adds `packages: write` when the image lives on `ghcr.io`.
`cibox update` only scaffolds the block into a workflow that has none, so a
user who widens the token keeps their version.

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
`# Required CI variables:` header at the top of the generated file lists.

### `cibox.ron` semantics

Omitted rules follow detection; an omitted `enabled` leaves detection in charge
while still applying the knobs.

### Docker rules

`image_name` has to be a valid docker reference, because it ends up as a literal
argument to `docker build -t`: lowercase alphanumerics separated by `.`, `-` or
`_`, optionally prefixed with a registry host and suffixed with a `:tag`.
Anything else is rejected by `cibox validate`. The default is derived from the
git remote (or the directory name) and normalized to fit, so a `Fossable/CiBox`
remote becomes `fossable/cibox`. The derived name is pushed to Docker Hub, which
only has `namespace/repository`, so a GitLab subgroup remote like
`group/subgroup/app` becomes `group/app`; setting `image_name` pushes elsewhere.

`docker-release` pushes the image under two tags of its own: `latest` and the
name of the git tag that triggered the release. An `image_name` that already
carries a `:tag` keeps it as a prefix on both — `example/app:edge` releases as
`example/app:edge-latest` and `example/app:edge-1.2.3` — since
`example/app:edge:latest` is not a reference docker would accept.

The registry is inferred from the image name. `docker-release` logs in to
`ghcr.io` with `GITHUB_TOKEN`, and everywhere else — the host prefix if the name
has one, Docker Hub otherwise — with the `DOCKER_USERNAME` and `DOCKER_PASSWORD`
secrets. If the credentials aren't configured in CI, the login is skipped, for
registries that don't require any. The `ghcr.io` user name is the runner's
`GITHUB_ACTOR` on GitHub and Gitea; GitLab and CircleCI have none, so there it
is the image's owner (`example` for `ghcr.io/example/app`), and `GITHUB_TOKEN`
is a personal access token the user adds as a project CI variable.

`sync_readme` additionally pushes `README.md` to the Docker Hub repository
description once the image is released, reusing the same two secrets. It is
Docker Hub only — `ghcr.io` has no description API — and like the login it is
skipped when the credentials aren't configured or there is no `README.md`.

#### Multi-arch images

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
`docker buildx` job, with QEMU set up when a foreign architecture is
involved. `WindowsAmd64` can't be emulated, so it gets its own job on a
Windows runner — that means GitHub Actions or Gitea Actions only (GitLab and
CircleCI refuse to generate), and the same Dockerfile has to work from a
Windows base image. Mixing Linux and Windows targets emits three jobs: the
two per-OS builds push staging tags (`:linux` and `:windows-amd64`, or
`:<tag>-linux` and `:<tag>-windows-amd64` when `image_name` carries a tag of
its own) and a third merges them into one multi-platform manifest under the
release tags.

### AUR publishing

`aur-release` publishes packages to the [AUR](https://aur.archlinux.org) when a
version tag is pushed. It is off by default — the AUR repo, not the project,
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

The job authenticates with the `AUR_SSH_PRIVATE_KEY` secret. The user generates
a dedicated key with `ssh-keygen -t ed25519`, adds the public half to their AUR
account, and stores the private half as a Secret (GitHub/Gitea) or a project CI
variable (GitLab/CircleCI). On GitLab it must be type Variable with variable
expansion turned off — the value is multi-line, which rules out masking, and
type File would hand the job a path instead of the key.

## TO-DO

- Write skill so agents can use cibox
- Add rules for agent harnesses
  - Cleanup TODOs
  - Run tests
  - Bump dependencies
  - mutation tests
  - benchmarks
- `go-release` (goreleaser) and multi-crate workspace publishing
