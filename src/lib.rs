//! devclean library: parallel scanning, artifact detection, filtering and
//! safe removal of build-artifact directories (`node_modules`, `target`,
//! `.venv`, caches of every popular ecosystem).

pub mod cleaner;
pub mod cli;
pub mod detector;
pub mod logging;
pub mod models;
pub mod paths;
pub mod scanner;
pub mod ui;
