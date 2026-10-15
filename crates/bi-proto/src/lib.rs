//! Blue Iris web server protocol, as used by UI3 (https://github.com/bp2008/ui3).
//!
//! No UI and no decoding here: just HTTP, JSON models, URL construction and the binary stream container.

pub mod api;
pub mod client;
pub mod commands;
pub mod lenient;
pub mod profile;
pub mod stream;
pub mod stream_parser;
pub mod stream_writer;
pub mod urls;

pub use client::{BiClient, Credentials, Session};
pub use stream_parser::{StreamEvent, StreamParser, VideoCodec};
