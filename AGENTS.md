`cibox` is a tool that generates CI/CD configurations for multiple platforms
(GitHub Actions, GitLab CI, CircleCI, Jenkins, Gitea) from detected project
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
  overrides, nix-services style (`rule_name: (enabled: bool, ...knobs)`).
  These types must stay hand-written literals — `build.rs` feeds this file to
  roniker for the RON LSP (`cibox lsp`).

To add a rule:

1. Add a struct + `impl Rule` in the matching `cibox/src/rules/<env>.rs`
   (or a new file), and register it in `resolve()` in `rules/mod.rs`.
2. Add a snake_case field for it to `Rules` in
   `cibox/src/config/ron_types.rs` (type `RuleToggle`, or a dedicated struct
   if the rule has knobs) and extend `enabled_override` /
   `set_enabled_override`. A unit test checks the id ↔ field mapping.
3. Give it a doc comment on the `Rules` field — roniker surfaces it in the
   LSP.

Jobs that need tools beyond a language toolchain should run on the
`fossable/cibox` image (`CIBOX_IMAGE`), built from the `Dockerfile` at the
repo root — add the tool there.

## TO-DO

- Write skill so agents can use cibox
- Add rules for agent harnesses
  - Cleanup TODOs
  - Run tests
  - Bump dependencies
  - mutation tests
  - benchmarks
- `go-release` (goreleaser) and multi-crate workspace publishing
