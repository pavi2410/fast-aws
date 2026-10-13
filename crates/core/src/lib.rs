//! Resource model, AWS clients, tasks and errors shared by every fast-aws service.

pub mod context;
pub mod error;
pub mod profiles;

pub use context::{CallerIdentity, ConfigFactory, Ctx, caller_identity};
pub use error::{AwsError, ErrorKind};
