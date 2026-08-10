//! CLI command implementations for the `anubis` binary.
//!
//! The library's [`anubis::scaffold`](anubis::scaffold) module holds the pure
//! text-transformation engine; these modules are its thin I/O shell: they
//! discover files, read and write the filesystem, probe the environment, and
//! render terminal output. Keeping side effects here keeps the engine
//! trivially unit-testable and the binary honest about what touches disk.

pub(crate) mod doctor;
pub(crate) mod new;
pub(crate) mod routes;
