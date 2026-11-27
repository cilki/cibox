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

The downside is, of course, you don't get the full flexibility of writing your
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
like `rust-test`, `rust-release`, or `docker-build`. Rules turn on
automatically based on what's in your project — a `Cargo.toml` enables the Rust
rules, a `Dockerfile` enables the Docker rules — and each one contributes jobs
to the generated pipeline. Build rules run on every push and upload their
outputs as artifacts; release rules run when you push a `v*` git tag.

Running `cibox update` in a project with no configuration at all produces a
working pipeline:

```
$ cibox                             # interactive TUI to toggle rules
$ cibox detect                      # show project facts and which rules fired
$ cibox validate                    # check cibox.ron and print the resolved rules
$ cibox update                      # write or refresh the pipeline files
$ cibox update --gitlab             # generate for a specific platform
$ cibox update --github --gitlab    # ...or for several at once
```

`cibox update` is safe to re-run after you customize the generated files: your
comments, formatting, and any jobs you added are kept, and only the jobs cibox
manages are brought up to date.

Generated pipelines are locked down by default: CI tokens get read-only access
unless a job needs more, and publishing credentials are only ever read from
your CI platform's secrets.

## Configuration

`cibox.ron` holds only your _overrides_ from the detected defaults — no file is
needed until you want to change something:

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

Beyond the basics, rules cover things like multi-arch Docker images and
publishing packages to the [AUR](https://aur.archlinux.org).

You can use our TUI interface to edit this file or any editor with LSP support.
Configure your editor to use `cibox lsp` as an LSP and you'll get inline
documentation and autocomplete.

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
