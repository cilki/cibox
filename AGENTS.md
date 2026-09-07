`cibox` is a tool that generates CI/CD configurations for multiple platforms
(GitHub Actions, GitLab CI, CircleCI, Jenkins) from a common specification. It
uses Rust structs as "presets" that can be composed into a final template.

## Architecture

Presets describe their jobs once in a platform-neutral IR (`cibox/src/ir.rs`:
`Job`, `Step`, `ToJobs`); per-platform backends in
`cibox/src/platforms/*/lower.rs` lower the IR to each platform's config model.
Steps are shell commands only — platform idioms (checkout actions, caching,
artifacts, job-key namespacing, triggers) are owned by the backends.

To add a preset:

1. Create `cibox/src/presets/<name>.rs` with a `#[derive(Preset)]` struct
   (bool fields defaulting to `false` keep it opt-in) and an `impl ToJobs`.
2. Add a `pub mod`/`pub use` line and one tuple to `with_presets!` in
   `cibox/src/presets/mod.rs`.
3. Add a variant to `PresetChoice` in `cibox/src/config/ron_types.rs` (this
   enum must stay literal for the RON LSP).

Jobs that need tools beyond a language toolchain should run on the
`fossable/cibox` image (`CIBOX_IMAGE`), built from the `Dockerfile` at the
repo root — add the tool there.

## TO-DO

- Write skill so agents can use cibox
- Add presets for agent harnesses
  - Cleanup TODOs
  - Run tests
  - Bump dependencies
  - mutation tests
  - benchmarks
