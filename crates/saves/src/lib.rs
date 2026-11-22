//! Backing up game saves to a saves server (default `saves.quad4.io`).
//!
//! * [`key`]: the secure key, an Ed25519 seed that is the account.
//! * [`client`]: sign-in and the snapshot routes of the HTTP API.
//! * [`snapshot`]: files into a hashed manifest and onto the server, with the
//!   limits checked first (the ones the server reports).
//! * [`manifest`]: the list of games and their save paths (Ludusavi's).
//! * [`local`]: backups in a folder on this PC, with no server.
//! * [`locations`]: those paths on this PC, and snapshot paths back onto it.
//!
//! No GPUI and no settings here: `heronite-emulation` joins this with the
//! game's own facts and the app's data files.

pub mod client;
pub mod error;
pub mod hash;
pub mod key;
pub mod local;
pub mod locations;
pub mod manifest;
pub mod snapshot;

pub use client::{login, normalize_server, Client, Login, Me, SnapshotInfo, DEFAULT_SERVER};
pub use error::{Result, SavesError};
pub use key::SecureKey;
