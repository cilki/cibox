pub mod platform;
pub mod ron_types;

pub use platform::Platform;
pub use ron_types::*;

/// RON options shared by every cibox.ron read and write.
///
/// `UNWRAP_VARIANT_NEWTYPES` keeps preset entries single-parenthesized
/// (`Rust(enable_linter: true)` instead of `Rust((enable_linter: true))`)
/// and `IMPLICIT_SOME` lets optional fields be written without `Some(...)`.
pub fn ron_options() -> ron::Options {
    ron::Options::default().with_default_extension(
        ron::extensions::Extensions::IMPLICIT_SOME
            | ron::extensions::Extensions::UNWRAP_VARIANT_NEWTYPES,
    )
}
