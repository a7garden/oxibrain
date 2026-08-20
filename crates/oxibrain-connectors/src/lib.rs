#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod html;
pub mod markdown;
pub mod oxios;
pub mod watch;
pub use html::{HtmlFrontmatterSplit, html_note_to_text, html_to_text, split_frontmatter};
pub use markdown::{MarkdownFile, scan_directory};
pub use oxios::{OxiosMemoryEntry, read_oxios_memory};
pub use watch::spawn_quiet;
