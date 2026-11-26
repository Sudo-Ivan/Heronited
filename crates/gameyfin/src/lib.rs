//! Gameyfin servers and their games.
//!
//! A server profile holds an address and an API key sealed for this user.
//! [`sync`] copies the server's games into the Collection as local entries;
//! PC games can then be downloaded and installed by [`install`]. Several
//! servers can be used at once: every game id is namespaced by server id.

pub mod client;
pub mod config;
pub mod install;
pub mod sync;

pub use client::{Client, Game, Image, Provider};
pub use config::{Server, Servers};
pub use install::{install, InstallOutcome};
pub use sync::{remove_server_entries, sync, Report};

pub const SOURCE: &str = "gameyfin";
pub const LAUNCH_KIND: &str = "gameyfin";
pub const SERVER_KEY: &str = "gameyfinServer";
pub const GAME_KEY: &str = "gameyfinGame";
pub const PROVIDER_KEY: &str = "gameyfinProvider";

/// The Collection's app name of `game` on `server`.
pub fn app_name(server: &str, game: &str) -> String {
    let clean = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect::<String>();
    format!("gameyfin_{}_{}", clean(server), clean(game))
}
