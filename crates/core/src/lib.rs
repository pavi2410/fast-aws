//! Resource model, AWS clients, tasks and errors shared by every fast-aws service.

pub mod context;
pub mod error;
pub mod profiles;
pub mod resource;

pub use context::{CallerIdentity, ConfigFactory, Ctx, caller_identity, default_region};
pub use error::{AwsError, ErrorKind};
pub use resource::{
    Cell, Column, ColumnWidth, Detail, ListQuery, Page, Registry, ResourceKind, Row, Safety, Scope,
    Tone,
};
