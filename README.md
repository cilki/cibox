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
- Jenkins
- Gitea

## How it works

cibox is built around **rules**: small, opinionated units of CI configuration
like `rust-test`, `rust-release`, or `docker-build`. Each rule is triggered by
detection — a `Cargo.toml` enables the Rust rules, a `Dockerfile` enables the
Docker rules, a publishable package enables its release rule — and contributes
jobs to the generated pipeline. Release rules run only on `v*` git tags (e.g.
`rust-release` runs `cargo publish`).

Running `cibox generate` in a project with no configuration at all produces a
working pipeline: detection picks the rules and the target platform is
inferred from existing CI files or your git remote.

```
$ cibox detect      # show project facts and which rules fired
$ cibox generate    # write the pipeline files
$ cibox             # interactive TUI to toggle rules
```

## `cibox.ron`

`cibox.ron` holds only your *overrides* from the detected defaults — no file
is needed until you want to change something. Each rule has an `enabled`
switch (like NixOS services) plus a few knobs where they make sense:

```ron
(
    platform: GitHub,  // optional; inferred when omitted
    rules: (
        // Force a rule off that detection enabled
        rust_audit: (enabled: false),

        // Force a rule on that detection missed
        go_lint: (enabled: true),

        // Set a knob without touching enablement
        docker_build: (image_name: "example/app"),
    ),
)
```

Omitted rules follow detection; an omitted `enabled` leaves detection in
charge while still applying the knobs.

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

## Rules

| Rule | Triggered by | Jobs |
|---|---|---|
| `rust-test` | `Cargo.toml` | `cargo test --all-features` |
| `rust-fmt` | `Cargo.toml` | `cargo fmt -- --check` |
| `rust-clippy` | `Cargo.toml` | `cargo clippy --all-features -- -D warnings` |
| `rust-audit` | `Cargo.toml` | `cargo audit` |
| `rust-release` | publishable `[package]` | `cargo publish` on `v*` tags (needs `CARGO_REGISTRY_TOKEN`) |
| `python-test` | `pyproject.toml` / `requirements.txt` | `pytest` |
| `python-lint` | " | `ruff check .` |
| `python-fmt` | " | `ruff format --check .` |
| `python-release` | `[project]` in pyproject.toml | build + `twine upload` on `v*` tags (needs `TWINE_PASSWORD`) |
| `go-test` | `go.mod` | `go test ./...` |
| `go-build` | `go.mod` | `go build ./...` |
| `go-lint` | `go.mod` | `golangci-lint run` |
| `go-audit` | `go.mod` | `gosec ./...` |
| `docker-build` | `Dockerfile` | `docker build` |
| `docker-release` | `Dockerfile` | build + push on `v*` tags (`ghcr.io/` images use `GITHUB_TOKEN`, others Docker Hub credentials) |
| `gitleaks` | git repository | secret scan over full history |

On GitHub/Gitea, tag-triggered rules land in a separate
`.github/workflows/release.yml`; other platforms gate them within the single
pipeline file. Platforms other than GitHub read the required secrets from
their own CI variable mechanisms — the generated file lists them in a header
comment.
