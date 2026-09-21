use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// The mapping is not a TenzorBus ring, or is a version this build cannot read.
    BadHeader(&'static str),
    /// No reclaimable slot appeared before the deadline under the `block` policy.
    RingFull,
    /// No publication arrived before the deadline.
    Timeout,
    /// Registry is full, or a second producer tried to attach.
    Capacity(&'static str),
    /// Caller passed something the protocol cannot represent.
    Invalid(String),
    /// A slot was recycled while a lease was still outstanding. This is a bug,
    /// never a normal condition; the invariant tests assert it never fires.
    LeaseViolation(String),
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::BadHeader(m) => write!(f, "invalid TenzorBus header: {m}"),
            Error::RingFull => write!(f, "no reclaimable slot before timeout"),
            Error::Timeout => write!(f, "no tensor published before timeout"),
            Error::Capacity(m) => write!(f, "capacity: {m}"),
            Error::Invalid(m) => write!(f, "invalid argument: {m}"),
            Error::LeaseViolation(m) => write!(f, "lease violation: {m}"),
            Error::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
