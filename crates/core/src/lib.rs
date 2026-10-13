//! Resource model, AWS clients, tasks and errors shared by every fast-aws service.

pub mod error;
pub mod profiles;

pub use error::{AwsError, ErrorKind};
