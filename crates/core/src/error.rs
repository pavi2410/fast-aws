//! Errors from AWS calls, classified so a view can explain them inside its pane
//! instead of showing a raw SDK error.

use std::error::Error as StdError;
use std::fmt;

use aws_smithy_runtime_api::client::result::SdkError;
use aws_smithy_types::error::display::DisplayErrorContext;
use aws_smithy_types::error::metadata::ProvideErrorMetadata;

/// What kind of failure this is, which decides how the pane presents it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The caller lacks permission for the action.
    AccessDenied,
    /// The SSO or `aws login` session has expired or was never started; `L` fixes it.
    SessionExpired,
    /// Credentials are missing, invalid or expired (non-SSO).
    Credentials,
    /// The API is rate limiting us; retries are already in progress.
    Throttled,
    /// The resource doesn't exist (any more).
    NotFound,
    /// The region or endpoint isn't usable from this account.
    Endpoint,
    /// The request never reached AWS (DNS, TLS, timeout, offline).
    Network,
    Other,
}

/// An AWS failure, reduced to what the UI needs.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub struct AwsError {
    pub kind: ErrorKind,
    /// The service error code, such as `UnauthorizedOperation`, when there is one.
    pub code: Option<String>,
    pub message: String,
}

impl fmt::Display for AwsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.code {
            Some(code) => write!(f, "{code}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl AwsError {
    pub fn new(kind: ErrorKind, code: Option<String>, message: impl Into<String>) -> Self {
        Self {
            kind,
            code,
            message: message.into(),
        }
    }

    /// The resource asked for doesn't exist.
    pub fn not_found(what: &str) -> Self {
        Self::new(ErrorKind::NotFound, None, format!("{what} not found"))
    }

    /// Builds an error from an SDK result, using the service error code when AWS sent
    /// one and the full error chain text otherwise.
    pub fn from_sdk<E, R>(err: SdkError<E, R>) -> Self
    where
        E: ProvideErrorMetadata + StdError + Send + Sync + 'static,
        R: fmt::Debug,
    {
        let (code, service_message) = match &err {
            SdkError::ServiceError(service) => {
                let e = service.err();
                (e.code().map(str::to_owned), e.message().map(str::to_owned))
            }
            _ => (None, None),
        };
        let network = matches!(err, SdkError::TimeoutError(_));
        let message = service_message.unwrap_or_else(|| DisplayErrorContext(&err).to_string());
        let kind = if network {
            ErrorKind::Network
        } else {
            classify(code.as_deref(), &message)
        };
        Self {
            kind,
            code,
            message,
        }
    }
}

/// Classifies a failure from its error code and message.
///
/// Credential failures happen before a request is sent, so they have no code and are
/// recognised from the error chain text produced by `aws-config`.
pub fn classify(code: Option<&str>, message: &str) -> ErrorKind {
    if let Some(code) = code {
        let kind = match code {
            "AccessDenied"
            | "AccessDeniedException"
            | "UnauthorizedOperation"
            | "UnauthorizedException"
            | "AuthorizationError"
            | "Forbidden" => Some(ErrorKind::AccessDenied),
            "ExpiredToken"
            | "ExpiredTokenException"
            | "RequestExpired"
            | "InvalidClientTokenId"
            | "UnrecognizedClientException"
            | "AuthFailure"
            | "SignatureDoesNotMatch" => Some(ErrorKind::Credentials),
            "Throttling"
            | "ThrottlingException"
            | "ThrottledException"
            | "RequestLimitExceeded"
            | "TooManyRequestsException"
            | "ProvisionedThroughputExceededException"
            | "SlowDown" => Some(ErrorKind::Throttled),
            "OptInRequired" | "InvalidRegion" => Some(ErrorKind::Endpoint),
            _ if code.ends_with("NotFound")
                || code.ends_with("NotFoundException")
                || code.starts_with("NoSuch") =>
            {
                Some(ErrorKind::NotFound)
            }
            _ => None,
        };
        if let Some(kind) = kind {
            return kind;
        }
    }

    let text = message.to_ascii_lowercase();
    // Specific network failures first: an SSO profile that is merely offline fails
    // with a chain that mentions both the SSO token and the network error, and
    // "log in again" would be the wrong advice.
    let network = [
        "dns error",
        "connection refused",
        "tcp connect",
        "timed out",
        "timeout",
    ];
    if network.iter().any(|m| text.contains(m)) {
        ErrorKind::Network
    } else if (text.contains("sso") && (text.contains("token") || text.contains("session")))
        || text.contains("login token")
        || text.contains("/login/cache")
    {
        ErrorKind::SessionExpired
    } else if text.contains("credentials") {
        ErrorKind::Credentials
    } else if text.contains("dispatch failure") {
        // Generic, so last: the missing-SSO-token chain contains it too.
        ErrorKind::Network
    } else {
        ErrorKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_codes_are_classified() {
        assert_eq!(
            classify(Some("UnauthorizedOperation"), ""),
            ErrorKind::AccessDenied
        );
        assert_eq!(
            classify(Some("AccessDeniedException"), ""),
            ErrorKind::AccessDenied
        );
        assert_eq!(classify(Some("ExpiredToken"), ""), ErrorKind::Credentials);
        assert_eq!(
            classify(Some("RequestLimitExceeded"), ""),
            ErrorKind::Throttled
        );
        assert_eq!(
            classify(Some("ThrottlingException"), ""),
            ErrorKind::Throttled
        );
        assert_eq!(classify(Some("OptInRequired"), ""), ErrorKind::Endpoint);
        assert_eq!(
            classify(Some("InvalidInstanceID.NotFound"), ""),
            ErrorKind::NotFound
        );
        assert_eq!(classify(Some("NoSuchBucket"), ""), ErrorKind::NotFound);
        assert_eq!(
            classify(Some("ResourceNotFoundException"), ""),
            ErrorKind::NotFound
        );
    }

    #[test]
    fn console_login_failures_are_recognised_from_the_chain_text() {
        // Messages produced by aws-config 1.12's `aws login` credentials provider.
        for msg in [
            "failed to load credentials: cached Login token is expired",
            "failed to load credentials: failed to read `/home/u/.aws/login/cache/ab12.json`",
            "failed to load credentials: failed to refresh cached Login token",
        ] {
            assert_eq!(classify(None, msg), ErrorKind::SessionExpired, "{msg}");
        }
    }

    #[test]
    fn sso_failures_are_recognised_from_the_chain_text() {
        // Messages produced by aws-config 1.12's SSO token provider.
        for msg in [
            "failed to load credentials: the SSO token has expired and cannot be refreshed",
            "an error occurred while loading credentials: failed to load the cached SSO token",
        ] {
            assert_eq!(classify(None, msg), ErrorKind::SessionExpired, "{msg}");
        }
    }

    #[test]
    fn an_offline_sso_profile_is_a_network_problem_not_an_expired_session() {
        let msg = "dispatch failure: other: an error occurred while loading credentials: \
                   call to SSO OIDC CreateToken for SSO token refresh failed: \
                   dispatch failure: io error: dns error: failed to lookup address";
        assert_eq!(classify(None, msg), ErrorKind::Network);
    }

    #[test]
    fn a_missing_sso_token_is_expired_despite_the_dispatch_failure_wrapper() {
        // Captured from aws-config 1.12 with no token in ~/.aws/sso/cache.
        let msg = "dispatch failure: other: an error occurred while loading credentials: \
                   an error occurred while loading an access token: \
                   failed to load the cached SSO token";
        assert_eq!(classify(None, msg), ErrorKind::SessionExpired);
    }

    #[test]
    fn other_credential_and_network_failures() {
        assert_eq!(
            classify(None, "dispatch failure: no credentials in the property bag"),
            ErrorKind::Credentials
        );
        assert_eq!(
            classify(None, "dispatch failure: dns error: failed to lookup"),
            ErrorKind::Network
        );
        assert_eq!(classify(Some("SomethingNew"), "boom"), ErrorKind::Other);
    }

    #[test]
    fn display_includes_the_code() {
        let err = AwsError::new(
            ErrorKind::AccessDenied,
            Some("UnauthorizedOperation".into()),
            "nope",
        );
        assert_eq!(err.to_string(), "UnauthorizedOperation: nope");
    }
}
