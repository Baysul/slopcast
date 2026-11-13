//! Slopcast's presenter core: the parts of the desktop application that are
//! neither GUI toolkit code nor media engine code.
//!
//! The core owns the presenter session for the lifetime of the process. Shells
//! (the native desktop and test drivers) send it typed
//! [`Command`]s and render the [`Snapshot`]s and [`Event`]s it publishes.

pub mod api;
pub mod app_icons;
pub mod audio;
pub mod bitrate;
pub mod codecs;
pub mod config;
pub mod media;
pub mod platform;
pub mod preferences;
pub mod presenter;
pub mod settings;
pub mod shortcuts;
pub mod storage;
pub mod telemetry;

pub use presenter::{
    CaptureStage, Command, Confirmation, DesktopAction, EndpointAvailability, Event, Notice,
    NoticeLevel, Presenter, RoomConnection, Snapshot,
};
