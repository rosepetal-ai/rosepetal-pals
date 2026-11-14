//! The parts of the A2A server that are PALS's own: [`SqliteTaskStore`], the
//! SDK's task store on the agent's SQLite database.

mod task_store;

pub use task_store::SqliteTaskStore;
