pub mod infer;
pub mod platform;
pub mod ron_types;

pub use infer::infer_platform;
pub use platform::Platform;
pub use ron_types::*;

/// RON options shared by every cibox.ron read and write.
///
/// `IMPLICIT_SOME` lets optional fields be written without `Some(...)`,
/// e.g. `enabled: false` instead of `enabled: Some(false)`.
pub fn ron_options() -> ron::Options {
    ron::Options::default()
        .with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME)
}
