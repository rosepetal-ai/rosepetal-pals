//! The parts of the A2A server that are PALS's own: the public Agent Card
//! ([`canonical_card`]) and the view of it the local gateway serves
//! ([`gateway_view`]), grant authentication in front of the SDK's routers
//! ([`auth`]), the ownership of tasks and the idempotency of messages
//! ([`GuardedHandler`], with the executor binding each execution to its
//! request when it starts) and [`SqliteTaskStore`], the SDK's task store on
//! the agent's SQLite database.

pub mod auth;
mod binding;
mod card;
mod guard;
mod task_store;

pub(crate) use binding::BindingExecutor;
pub use card::{BINDING_URI, SECURITY_SCHEME, canonical_card, gateway_view};
pub use guard::GuardedHandler;
pub use task_store::SqliteTaskStore;
