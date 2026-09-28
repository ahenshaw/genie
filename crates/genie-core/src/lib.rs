//! Genie's family tree model, shared by the app and the server: reading and
//! writing GEDCOM, people and families, relationships, documents, `.gdz`
//! bundles, merging edits from several people, and what guests may see.
//! Nothing here draws anything.

pub mod bundle;
pub mod dedup;
pub mod gedcom;
pub mod kinship;
pub mod media;
pub mod model;
pub mod privacy;
pub mod sync;

/// A small example tree, for trying Genie out and for tests.
pub const SAMPLE_GED: &[u8] = include_bytes!("sample.ged");
