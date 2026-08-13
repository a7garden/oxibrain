//! oxibrain-index: pure retrieval algorithms (RRF, TF-IDF, kNN, adjacency,
//! community label propagation). No rusqlite, no I/O — all algorithms are pure
//! functions over in-memory data structures.

#![cfg_attr(test, allow(clippy::unwrap_used))]
pub mod ngram;
pub mod rrf;
pub mod spec;
pub mod vector;
pub use ngram::{jaccard, lsh_bands, minhash, shingle_entropy, shingles};
pub use rrf::{FusedItem, fuse};
pub use spec::{Direction, PredicateFilter};
pub use vector::{TfIdfModel, TfIdfVector, cosine_sim, features};
pub mod adjacency;
pub mod knn;
pub use adjacency::{AdjacencyGraph, BfsResult, BfsSpec};
pub use knn::KnnIndex;
pub mod community;
pub use community::{CommunityMap, label_propagation};
