use std::time::Duration;

/// Everything isb can fail with.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Could not reach incusd at all (socket missing, permission denied, refused).
    #[error("cannot connect to incusd at {socket}: {source}")]
    Connect {
        socket: String,
        #[source]
        source: std::io::Error,
    },

    /// A single HTTP request to incusd did not complete within its socket timeout.
    #[error("incusd did not answer {method} {path} within {timeout:?}")]
    RequestTimeout {
        method: String,
        path: String,
        timeout: Duration,
    },

    /// incusd answered with an error response.
    #[error("incusd: {method} {path}: {message} (HTTP {status})")]
    Api {
        method: String,
        path: String,
        status: u16,
        message: String,
    },

    /// A background operation ran past its deadline. `step` names what isb was
    /// doing ("create instance", "start instance", ...), so a stall is reported
    /// as the step that stalled rather than as a bare hang.
    #[error(
        "{step} stalled: operation {operation} still {status} after {waited:?}{}",
        if *.cancelled { " (cancelled)" } else { " (not cancellable; left running on the server)" }
    )]
    OperationTimeout {
        step: String,
        operation: String,
        status: String,
        waited: Duration,
        cancelled: bool,
    },

    /// A background operation finished unsuccessfully.
    #[error("{step} failed: {message}")]
    OperationFailed { step: String, message: String },

    /// A readiness check did not pass before its deadline.
    #[error("sandbox {sandbox} not ready after {waited:?}: {check} ({detail})")]
    NotReady {
        sandbox: String,
        check: String,
        detail: String,
        waited: Duration,
    },

    /// An exec with an explicit timeout ran past it; the process was sent SIGKILL.
    #[error("exec {argv} timed out after {timeout:?} (killed)")]
    ExecTimeout { argv: String, timeout: Duration },

    #[error("sandbox {0} not found")]
    NotFound(String),

    #[error("sandbox {0} already exists")]
    AlreadyExists(String),

    /// The spec, the compose file or an argument is invalid.
    #[error("{0}")]
    Invalid(String),

    /// Variable interpolation failed (unset variable, `${VAR:?message}`, bad syntax).
    #[error("interpolation: {0}")]
    Interpolation(String),

    #[error("{path}: {message}")]
    Parse { path: String, message: String },

    #[error("websocket: {0}")]
    WebSocket(String),

    #[error("protocol: {0}")]
    Protocol(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl Error {
    pub(crate) fn invalid(msg: impl Into<String>) -> Self {
        Error::Invalid(msg.into())
    }

    /// True for an incusd "not found" answer.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Error::Api { status: 404, .. } | Error::NotFound(_))
    }

    /// True for an incusd conflict (the object already exists).
    pub fn is_conflict(&self) -> bool {
        matches!(self, Error::Api { status: 409, .. } | Error::AlreadyExists(_))
            || matches!(self, Error::Api { message, .. } if message.contains("already exists"))
    }

    /// True when the error is a deadline, either on a request or an operation.
    pub fn is_timeout(&self) -> bool {
        matches!(
            self,
            Error::RequestTimeout { .. } | Error::OperationTimeout { .. }
        )
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
