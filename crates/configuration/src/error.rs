pub type Result<T> = core::result::Result<T, Error>;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Invalid format: {0}")]
    InvalidFormat(String),
    #[error("Validation failed: {0}")]
    Validation(String),
    #[error("Failed to solve dependency: {0}")]
    DependencySolvation(String),

    #[error("Constraint violation: {0}")]
    ConstraintError(String),

    #[error("Duration parse error: {0}")]
    ParseDurationError(#[from] humantime::DurationError),

    #[error("Setting can not be both dynamic and immutable")]
    DynamicAndImmutable,
    #[error("Setting can only have immutable dependency")]
    DependencyNotImmutable
}