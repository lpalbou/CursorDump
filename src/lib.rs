//! CursorDump library: scanning, parsing, search, export and the local web
//! server. The binary (`main.rs`) is a thin shell over these modules.

pub mod backup;
pub mod export;
pub mod media;
pub mod model;
pub mod parser;
pub mod scanner;
pub mod search;
pub mod server;
