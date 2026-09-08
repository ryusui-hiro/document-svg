use std::fmt::{Display, Formatter};
use std::io;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    InvalidInput(String),
    Unsupported(String),
    LimitExceeded(String),
    Pdf(lopdf::Error),
    Xml(quick_xml::Error),
    Zip(zip::result::ZipError),
    Json(serde_json::Error),
}

impl Display for Error {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "I/O error: {error}"),
            Self::InvalidInput(message) => write!(formatter, "invalid input: {message}"),
            Self::Unsupported(message) => write!(formatter, "unsupported input: {message}"),
            Self::LimitExceeded(message) => write!(formatter, "safety limit exceeded: {message}"),
            Self::Pdf(error) => write!(formatter, "PDF error: {error}"),
            Self::Xml(error) => write!(formatter, "XML error: {error}"),
            Self::Zip(error) => write!(formatter, "ZIP error: {error}"),
            Self::Json(error) => write!(formatter, "JSON error: {error}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<lopdf::Error> for Error {
    fn from(value: lopdf::Error) -> Self {
        Self::Pdf(value)
    }
}

impl From<quick_xml::Error> for Error {
    fn from(value: quick_xml::Error) -> Self {
        Self::Xml(value)
    }
}

impl From<zip::result::ZipError> for Error {
    fn from(value: zip::result::ZipError) -> Self {
        Self::Zip(value)
    }
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
