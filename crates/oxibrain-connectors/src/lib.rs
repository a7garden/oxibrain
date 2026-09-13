#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod decode;
pub mod documents_config;
pub mod git_docs;
pub mod html;
pub mod markdown;
pub mod oxios;
pub mod pdc;
pub mod scan;
pub mod watch;
pub use decode::{DECODER_VERSION, DecodedDocument, MediaType, decode};
pub use documents_config::{
    CONFIG_FILE_NAME, ConfigError, DocumentsConfig, RootEntry, UpsertOutcome,
};
pub use git_docs::{DocumentRevision, GitBlob, GitDocumentReader, GitSnapshot};
pub use html::{HtmlFrontmatterSplit, html_note_to_text, html_to_text, split_frontmatter};
pub use markdown::{MarkdownFile, scan_directory};
pub use oxibrain_core::documents::FileObservation;
pub use oxios::{OxiosMemoryEntry, read_oxios_memory};
pub use pdc::{
    PDC_CORPUS_REVISION, PdcBody, PdcDiagnostic, PdcDiagnosticCode, PdcDocument, PdcLink,
    PdcMetadata, PdcTask, classify_html_transport, parse_djot_document, parse_html_document,
};
pub use scan::{ScanResult, SkippedFile, canonicalize_root, scan_root};
