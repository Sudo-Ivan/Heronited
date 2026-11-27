//! GameHub servers and their games.
//!
//! A server profile holds an address. [`sync`] copies the server's games into
//! the Collection as local entries; PC games can then be downloaded and
//! installed by [`install`]. Several servers can be used at once: every game
//! id is namespaced by server id.

pub mod client;
pub mod config;
pub mod install;
pub mod sync;

pub use client::{Client, Game, GameFile, OsSupport};
pub use config::{Server, Servers};
pub use install::{install, InstallOutcome};
pub use sync::{remove_server_entries, sync, Report};

pub const SOURCE: &str = "gamehub";
pub const LAUNCH_KIND: &str = "gamehub";
pub const SERVER_KEY: &str = "gamehubServer";
pub const GAME_KEY: &str = "gamehubGame";
pub const VERSION_KEY: &str = "gamehubVersion";

/// The Collection's app name of `game` on `server`.
pub fn app_name(server: &str, game: &str) -> String {
    let clean = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect::<String>();
    format!("gamehub_{}_{}", clean(server), clean(game))
}
