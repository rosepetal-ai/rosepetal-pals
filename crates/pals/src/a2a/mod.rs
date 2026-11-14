//! The parts of the A2A server that are PALS's own: the public Agent Card
//! ([`canonical_card`]), grant authentication in front of the SDK's routers
//! ([`auth`]), the ownership of tasks ([`GuardedHandler`]) and
//! [`SqliteTaskStore`], the SDK's task store on the agent's SQLite database.

pub mod auth;
mod card;
mod guard;
mod task_store;

pub use card::{BINDING_URI, SECURITY_SCHEME, canonical_card};
pub use guard::GuardedHandler;
pub use task_store::SqliteTaskStore;
