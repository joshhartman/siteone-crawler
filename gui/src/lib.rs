//! SiteOne Crawler GUI — a native desktop front end for the `siteone-crawler` CLI.
//! See `docs/GUI-SPEC.md`.
//!
//! - [`model`]: configuration, option schema, events, run state, results, persistence (pure).
//! - [`backend`]: the crawler child process and one-shot tool calls (no egui).
//! - [`app`], [`theme`], [`ui`]: the eframe application.

pub mod app;
pub mod backend;
pub mod model;
pub mod theme;
pub mod ui;
