//! System tray user interface, icon rendering, and desktop event loop.
//!
//! Provides a tray icon residing in the Windows notification area that reflects
//! synchronization status across configured destination paths, provides access to logs
//! and configuration, and offers manual sync triggers.

pub(crate) mod assets;
pub(crate) mod dialog;
pub(crate) mod event_loop;
pub(crate) mod menu;
pub(crate) mod state;

pub use dialog::{format_explorer_args, open_path};
pub use event_loop::TrayEventLoop;
pub use menu::TrayActionHandler;
pub use state::{DestinationState, EngineStatus, TrayExitReason, TrayState};
