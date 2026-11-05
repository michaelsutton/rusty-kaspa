use thiserror::Error;

#[derive(Clone, Debug, Error)]
pub enum Error {
    #[error("the subscription key store reached the maximum capacity")]
    MaxCapacityReached,
}

pub type Result<T> = std::result::Result<T, Error>;
