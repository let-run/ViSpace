#[cfg(not(target_arch = "wasm32"))]
pub mod discovery;
#[cfg(not(target_arch = "wasm32"))]
mod network_auth;
#[cfg(not(target_arch = "wasm32"))]
mod network_security;
pub mod network_identity;
#[cfg(not(target_arch = "wasm32"))]
pub mod network_tls;
mod git_progress;
#[cfg(not(target_arch = "wasm32"))]
pub mod data_location;
#[cfg(not(target_arch = "wasm32"))]
pub mod model_library;
pub mod action_log;
#[cfg(not(target_arch = "wasm32"))]
pub mod api;
#[cfg(target_arch = "wasm32")]
#[path = "web/api.rs"]
pub mod api;
pub mod attachments;
pub mod csv_io;
pub mod merge_review;
pub mod mesh;
pub mod model;
#[cfg(not(target_arch = "wasm32"))]
pub mod store;
#[cfg(target_arch = "wasm32")]
#[path = "web/store.rs"]
pub mod store;
#[cfg(not(target_arch = "wasm32"))]
pub mod sync;
#[cfg(not(target_arch = "wasm32"))]
pub mod sync_blob;
pub mod topology;
#[cfg(not(target_arch = "wasm32"))]
mod sync_relay;
#[cfg(not(target_arch = "wasm32"))]
pub mod component_sync;
pub mod ui;

pub mod label;
pub mod notes;

pub mod description_analysis;

pub mod latex_document;
pub mod markdown_notes;
pub mod markdown_visual;
pub mod notes_pdf;

pub mod requests;

pub mod csv_import;
pub mod tasks;

pub mod async_io;

pub mod platform;
#[cfg(target_arch = "wasm32")]
pub mod web;

#[path = "web/jobs.rs"]
pub mod browser_jobs;

#[cfg(not(target_arch = "wasm32"))]
pub mod git_bundle;

pub mod kicad_sync;
#[cfg(not(target_arch = "wasm32"))]
pub mod kicad_watch;

pub mod api_settings;

pub mod purchase_import;

mod designator_priority;

pub mod pcb_cache;
pub mod project_cache;

#[cfg(all(test,not(target_arch="wasm32")))]
mod network_security_tests;
