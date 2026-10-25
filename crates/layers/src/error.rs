//! containerd's error kinds (`errdefs`) and the gRPC codes it sends them as (`errgrpc.ToGRPC`): a message is said as
//! containerd says it ("snapshot does not exist: not found"), so its clients read it as their own.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    NotFound(String),
    AlreadyExists(String),
    FailedPrecondition(String),
    InvalidArgument(String),
    Unavailable(String),
    /// Anything else (a file that could not be read, a mount that failed): gRPC's Unknown, as containerd sends it.
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// The same kind of error, its message after `what` ("failed to remove snapshot x: …").
    pub fn context(self, what: impl fmt::Display) -> Error {
        // (An error with nothing of its own to say says only what it is about.)
        let w = |m: String| if m.is_empty() { what.to_string() } else { format!("{what}: {m}") };
        match self {
            Error::NotFound(m) => Error::NotFound(w(m)),
            Error::AlreadyExists(m) => Error::AlreadyExists(w(m)),
            Error::FailedPrecondition(m) => Error::FailedPrecondition(w(m)),
            Error::InvalidArgument(m) => Error::InvalidArgument(w(m)),
            Error::Unavailable(m) => Error::Unavailable(w(m)),
            Error::Other(m) => Error::Other(w(m)),
        }
    }

    pub fn is_not_found(&self) -> bool { matches!(self, Error::NotFound(_)) }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let (m, kind) = match self {
            Error::NotFound(m) => (m, "not found"),
            Error::AlreadyExists(m) => (m, "already exists"),
            Error::FailedPrecondition(m) => (m, "failed precondition"),
            Error::InvalidArgument(m) => (m, "invalid argument"),
            Error::Unavailable(m) => (m, "unavailable"),
            Error::Other(m) => return f.write_str(m),
        };
        write!(f, "{m}: {kind}")
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self { Error::Other(e.to_string()) }
}

impl From<Error> for tonic::Status {
    fn from(e: Error) -> Self {
        use tonic::Code;
        let code = match &e {
            Error::NotFound(_) => Code::NotFound,
            Error::AlreadyExists(_) => Code::AlreadyExists,
            Error::FailedPrecondition(_) => Code::FailedPrecondition,
            Error::InvalidArgument(_) => Code::InvalidArgument,
            Error::Unavailable(_) => Code::Unavailable,
            Error::Other(_) => Code::Unknown,
        };
        tonic::Status::new(code, e.to_string())
    }
}

/// An error from the metadata database: none of containerd's kinds.
pub fn db(e: impl fmt::Display) -> Error { Error::Other(e.to_string()) }
