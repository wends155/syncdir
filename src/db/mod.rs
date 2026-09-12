//! SQLite-backed storage and caching subsystem for syncdir.
//!
//! Provides the `HashStore` trait, `FileRecord` metadata representation,
//! `StoreConfig` parameters, `MockHashStore` in-memory test double,
//! and concrete `SqliteHashStore` engine.

pub(crate) mod mock;
pub(crate) mod sqlite;
pub(crate) mod traits;

#[doc(hidden)]
pub use mock::{MockHashStore, MockStoreErrorHook};
pub use sqlite::SqliteHashStore;
pub use traits::{BlockHash, FileRecord, HashStore, StoreConfig};
