//! Resource model, AWS clients, tasks and errors shared by every fast-aws service.

pub mod arn;
pub mod config;
pub mod context;
pub mod error;
pub mod fmt;
pub mod profiles;
pub mod resource;

pub use config::{Config, Guard};
pub use context::{CallerIdentity, ConfigFactory, Ctx, caller_identity, default_region};
pub use error::{AwsError, ErrorKind};
pub use resource::{
    Cell, Column, ColumnWidth, Detail, Drill, ListQuery, Page, Parent, Query, Registry, Relation,
    ResourceKind, Row, Safety, Scope, Tone,
};
