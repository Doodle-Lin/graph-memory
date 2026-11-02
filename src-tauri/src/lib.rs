mod agent_connector;
pub mod engine;
mod importer;
mod llm_extract;
pub mod static_server;
pub mod service;
pub mod memory_store;
pub mod backup_format;
pub mod backup_io;

#[cfg(feature = "desktop")]
mod desktop;
#[cfg(feature = "desktop")]
pub use desktop::run;
