//! The model layer: pure data and logic, no UI, no child processes.

pub mod config;
pub mod console;
pub mod coverage;
pub mod events;
pub mod history;
pub mod link_check;
pub mod link_report;
pub mod link_text;
pub mod paths;
pub mod presets;
pub mod profiles;
pub mod results;
pub mod retry;
pub mod run_plan;
pub mod run_state;
pub mod schema;
pub mod secrets;
pub mod settings;
pub mod storage;

#[cfg(test)]
pub(crate) mod test_support;
