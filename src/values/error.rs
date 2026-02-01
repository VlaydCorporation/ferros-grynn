use thiserror::Error;

pub type Result<T> = core::result::Result<T, ValueError>;

#[derive(Error, Debug)]
pub enum ValueError {
    #[error("{0}")]
    NoSuchElement(&'static str),
}