//! Market data for Persephone: fetching from exchanges, the local tick store, and the CSV
//! importer that fills it.

pub mod archive;
pub mod binance;
pub mod import;
pub mod store;
