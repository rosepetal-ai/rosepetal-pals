//! The parts of the A2A server that are PALS's own: the signed Agent Cards,
//! public ([`canonical_card`]) and extended per scope ([`extended_card`]),
//! all signed when the node starts ([`CardSet`]), and the view of a card the
//! local gateway serves ([`gateway_view`]), grant authentication in front of
//! the SDK's routers ([`auth`]), the ownership of tasks and the idempotency
//! of messages ([`GuardedHandler`], with the executor binding each execution
//! to its request when it starts) and [`SqliteTaskStore`], the SDK's task
//! store on the agent's SQLite database.

pub mod auth;
mod binding;
mod card;
mod guard;
mod shims;
mod task_store;

pub(crate) use binding::BindingExecutor;
pub use card::{
    BINDING_URI_HTTP_JSON, BINDING_URI_JSONRPC, CardSet, DEFAULT_DESCRIPTION,
    IDEMPOTENCY_EXTENSION, SECURITY_SCHEME, SignedCard, canonical_card, extended_card,
    gateway_view,
};
pub use guard::GuardedHandler;
pub(crate) use shims::{echo_extensions, jsonrpc_shim, rest_shim};
pub use task_store::SqliteTaskStore;

/// The longest task id PALS accepts, from a peer or for its own tasks.
pub const MAX_TASK_ID_LEN: usize = 128;

/// Whether `task_id` is one PALS uses as it is, in a directory name or in a
/// URL path: 1 to [`MAX_TASK_ID_LEN`] ASCII letters, digits, `-` or `_`.
/// The SDK makes task ids UUIDs; a peer's task id that is anything else is
/// refused, never used.
pub fn is_safe_task_id(task_id: &str) -> bool {
    (1..=MAX_TASK_ID_LEN).contains(&task_id.len())
        && task_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_task_ids_are_short_and_plain() {
        for safe in [
            "01a1d91c-f621-7222-ab29-561b9ef4ae5a",
            "t_1",
            "a",
            &"x".repeat(128),
        ] {
            assert!(is_safe_task_id(safe), "{safe}");
        }
        for unsafe_id in [
            "",
            &"x".repeat(129),
            "../x",
            "a/b",
            "a:cancel",
            "a?b",
            "a%2Fb",
            "a b",
            "ñ",
            ".",
        ] {
            assert!(!is_safe_task_id(unsafe_id), "{unsafe_id}");
        }
    }
}
