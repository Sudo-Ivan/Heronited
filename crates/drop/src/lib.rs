//! Drop OSS servers and their games.

pub mod auth;
pub mod client;
pub mod config;
pub mod install;
pub mod sync;

pub use auth::Auth;
pub use client::{Client, Game, Launch, VersionDetails, VersionOption};
pub use config::{Server, Servers};
pub use install::{install, InstallOutcome};
pub use sync::{remove_server_entries, sync, Report};

pub const SOURCE: &str = "drop";
pub const LAUNCH_KIND: &str = "drop";
pub const SERVER_KEY: &str = "dropServer";
pub const GAME_KEY: &str = "dropGame";
pub const VERSION_KEY: &str = "dropVersion";
pub const PLATFORM_KEY: &str = "dropPlatform";
pub const LATEST_KEY: &str = "dropLatestVersion";
pub const SETUP_KEY: &str = "dropSetup";

/// The Collection's app name of `game` on `server`.
pub fn app_name(server: &str, game: &str) -> String {
    let clean = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect::<String>();
    format!("drop_{}_{}", clean(server), clean(game))
}
