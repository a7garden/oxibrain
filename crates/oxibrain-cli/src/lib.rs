//! Library surface for the `oxibrain` CLI. The binary (`main.rs`) re-exports
//! `Cli` / `Command` from this crate; integration tests reach into the
//! internal `cmd` modules directly to drive the resolution ladder against
//! hermetic fakes. Keeping the modules `pub` here means the production
//! binary does not gain a new public API (no version bump, no SemVer impact)
//! — the surface is consumed only by the in-crate tests directory.

pub mod cli;
pub mod cmd;
