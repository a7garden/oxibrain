//! oxibrain-index: pure retrieval algorithms (RRF, TF-IDF, kNN, adjacency,
//! community label propagation). No rusqlite, no I/O — all algorithms are pure
//! functions over in-memory data structures.

#![cfg_attr(test, allow(clippy::unwrap_used))]