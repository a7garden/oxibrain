#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod markdown;
pub use markdown::{MarkdownFile, scan_directory};
