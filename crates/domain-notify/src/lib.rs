//! `domain-notify`: notification policy and Web Push delivery (design doc
//! §10.4, D13).
//!
//! - [`policy`] decides at send time: mutes, 24-hour expiry, quiet hours,
//!   the daily limit, and calm-mode text.
//! - [`webpush`] encrypts messages (RFC 8291) and signs VAPID tokens
//!   (RFC 8292). Endpoints must be on a known push service.
//! - [`service::NotifyService`] queues one notification per new insight that
//!   can notify, and delivers due ones.

pub mod policy;
pub mod service;
pub mod webpush;

pub use service::{NotifyError, NotifyService};
