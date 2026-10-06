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
detection — a `Cargo.toml` enables the Rust rules, a `Dockerfile` enables the
Docker rules, a publishable package enables its release rule — and contributes
jobs to the generated pipeline. Release rules run only on `v*` git tags (e.g.
`rust-release` runs `cargo publish`).

Running `cibox update` in a project with no configuration at all produces a
working pipeline: detection picks the rules and the target platform is
inferred from existing CI configuration, then from your git remote's host,
falling back to GitHub Actions. Pass `--platform` to choose explicitly.

```
$ cibox                           # interactive TUI to toggle rules
$ cibox detect                    # show project facts and which rules fired
$ cibox validate                  # check cibox.ron and print the resolved rules
$ cibox update                    # write or refresh the pipeline files
$ cibox update --platform gitlab  # generate for a specific platform
```

The files cibox manages, per platform:

| Platform | Files |
|---|---|
| GitHub Actions | `.github/workflows/ci.yml`, `.github/workflows/release.yml` |
| Gitea Actions | `.gitea/workflows/ci.yml`, `.gitea/workflows/release.yml` |
| GitLab CI | `.gitlab-ci.yml` |
| CircleCI | `.circleci/config.yml` |

`cibox update` is safe to re-run after you customize the generated files. It
conforms the jobs cibox manages to its current output, removes managed jobs
whose rule you disabled, and leaves everything else alone: jobs you deleted
stay deleted, and jobs you added are kept. Pass `--force` to rewrite the
files from scratch instead.

### Token permissions

GitHub and Gitea workflows are generated with `permissions: contents: read`,
so the ambient CI token can check out the code and nothing else — without
that key the token inherits the repository or organization default, which can
be write-all. Jobs that genuinely need more ask for it individually:
`docker-release` adds `packages: write` when the image lives on `ghcr.io`.
`cibox update` only scaffolds the block into a workflow that has none, so if
you widen the token yourself your version is kept.

The generated checkout also passes `persist-credentials: false`. By default
`actions/checkout` writes the token into `.git/config` as an auth header,
where every later step can read it — including build scripts and test suites
running third-party code. Nothing cibox generates uses git after the clone,
so the credential is dropped.

## `cibox.ron`

`cibox.ron` holds only your *overrides* from the detected defaults — no file
is needed until you want to change something. Each rule has an `enabled`
switch (like NixOS services) plus a few knobs where they make sense:

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

Omitted rules follow detection; an omitted `enabled` leaves detection in
charge while still applying the knobs. The target platform is not part of
the file — `cibox update` can generate for any platform, so pass
`--platform` or let cibox infer it.

`image_name` has to be a valid docker reference, because it ends up as a
literal argument to `docker build -t`: lowercase alphanumerics separated by
`.`, `-` or `_`, optionally prefixed with a registry host and suffixed with a
`:tag`. Anything else is rejected by `cibox validate`. The default is derived
from the git remote (or the directory name) and normalized to fit, so a
`Fossable/CiBox` remote becomes `fossable/cibox`.

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
`docker buildx` job, with QEMU set up when a foreign architecture is
involved. `WindowsAmd64` can't be emulated, so it gets its own job on a
Windows runner — that means GitHub Actions or Gitea Actions only (GitLab and
CircleCI refuse to generate), and the same Dockerfile has to work from a
Windows base image. Mixing Linux and Windows targets emits three jobs: the
two per-OS builds push staging tags and a third merges them into one
multi-platform manifest, so `image_name` must not already carry a tag.

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

