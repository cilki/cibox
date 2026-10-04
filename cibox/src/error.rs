pub type Result<T> = anyhow::Result<T>;

pub fn unsupported_platform_error(platform: impl Into<String>) -> anyhow::Error {
    anyhow::anyhow!("Platform '{}' is not supported", platform.into())
}
