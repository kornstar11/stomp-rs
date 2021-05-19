use std::io::{Error as IoError};
use crate::frame::Frame;

#[derive(Debug)]
pub enum Error {
    Io(IoError),
    Stomp(String),
    Disconnected,
}

impl From<&IoError> for Error {
    fn from(e: &IoError) -> Self {
        Error::Io(IoError::from(e.kind()))
    }
}

impl From<&Frame> for Error {
    fn from(e: &Frame) -> Self {
        Error::Stomp(String::from_utf8_lossy(&e.body).to_string())
    }
}