//! The messages branches send each other, one struct each. Links in
//! bonsai.toml carry them from branch to branch. `bonsai message add` writes
//! one; add fields by hand whenever you like. Each derives Clone (one message
//! can go to several branches) and Debug (tests and logs print it).

#[allow(unused_imports)] // units (`temp: Celsius`), here and in every branch
pub use crate::bonsai::units::*;

// bonsai:message
