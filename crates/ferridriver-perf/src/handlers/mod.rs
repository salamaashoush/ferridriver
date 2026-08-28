//! Handlers turn the flat trace-event array into a model.
//!
//! Each one owns a single pass over the events and answers a single
//! question, mirroring devtools-frontend's `handlers/` directory.

pub mod meta;
pub mod network;
pub mod page_load;
