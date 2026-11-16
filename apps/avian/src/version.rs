//! Identity baked into the binary at build time.

pub const BUILD_VERSION: &str = env!("AVIAN_BUILD_VERSION");
pub const RELEASE_TAG: &str = env!("AVIAN_RELEASE_TAG");
pub const GIT_SHA: &str = env!("AVIAN_BUILD_SHA");
pub const GIT_DIRTY: bool = matches!(env!("AVIAN_BUILD_DIRTY").as_bytes(), b"true");
