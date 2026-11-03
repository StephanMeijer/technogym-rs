#![warn(missing_docs)]
//! An unofficial async client for Technogym's **mywellness** cloud: workout sessions, every
//! exercise with per-set reps and weight, the machine's full summary, per-second treadmill and
//! bike data, and whole-session heart rate.
//!
//! Technogym's official API is partner-only. This crate speaks the JSON API the end-user web
//! app uses, signing in with the account's email and password. It will break if Technogym
//! changes the login flow or its client-trust rules. Read-only: nothing here changes the
//! account.
//!
//! ```no_run
//! # async fn demo() -> technogym::Result<()> {
//! use technogym::{Credentials, SegmentOptions, Technogym};
//!
//! // Signs in lazily on the first call, and again whenever the token is rejected.
//! let tg = Technogym::builder().credentials(Credentials::new("me@example.com", "secret")).build();
//! for session in tg.get_recent_workouts(7).await? {
//!     println!("{} {} ({} exercises)", session.date, session.name, session.exercises.len());
//!     for run in session.exercises.iter().filter(|e| e.is_cardio) {
//!         let analyzed = tg.analyze_exercise(&run.analytics_id, SegmentOptions::default()).await?;
//!         println!("  {} segments by {:?}", analyzed.segments.len(), analyzed.segment_mode);
//!     }
//! }
//! # Ok(()) }
//! ```
//!
//! Every metric is a [`Property`] of `value` (base unit: seconds, metres, kg), `display_value`
//! (in `unit`, what the console shows) and `formatted` (the localised string). Names and units
//! come back in the account's culture.

pub mod analysis;
pub mod constants;
pub mod dates;
pub mod parse;
pub mod types;

mod client;
mod error;
mod session;
mod transport;

pub use analysis::{
    CUMULATIVE_CHANNELS, ChannelStats, CumulativeDelta, GradeBands, HrStats, SETPOINT_CHANNELS,
    Segment, SegmentBy, SegmentChannel, SegmentMode, SegmentOptions, acsm_vo2, add_heart_rate,
    channel_stats, downsample, elevation_gain, exercise_elevation_gain, grade_bands, hr_stats,
    sample_every, segment_analytics, window_samples,
};
pub use client::{AnalyzedExercise, Credentials, Technogym, TechnogymBuilder, is_auth_error};
pub use error::{Error, Result};
pub use session::{FileSessionStore, MemorySessionStore, Session, SessionStore};
pub use transport::TransportOptions;
pub use types::*;
