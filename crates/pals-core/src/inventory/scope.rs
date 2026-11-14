//! Scopes: which part of the inventory a requester sees.
//!
//! The scope comes from the grant the requester presented and from nothing
//! else it sends. Milestone 2 adds no grant operations (`agentnet.grant/1`
//! stays frozen): a grant whose resources cover `skill:ask` gives the view
//! of its project when it allows any task operation, and the view of a task
//! in that project when it allows `task.send`. Every other requester, with
//! no grant or with one for anything else, sees the public view.

use super::Visibility;
use crate::grant::{Grant, Operation};

/// The resource whose task operations open a project's views.
const ASK_RESOURCE: &str = "skill:ask";

/// The view a requester gets: public, or a project's, or a task's in a
/// project, each also seeing what the views before it see.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scope {
    Public,
    Project(String),
    Task(String),
}

impl Scope {
    /// The scope's name, as caches, views and the `x-agentnet-scope` header
    /// carry it: `public`, `project:<project>` or `task:<project>`.
    pub fn key(&self) -> String {
        match self {
            Scope::Public => "public".to_owned(),
            Scope::Project(project) => format!("project:{project}"),
            Scope::Task(project) => format!("task:{project}"),
        }
    }

    /// The scope `grant` gives: [`Scope::Task`] of its project when it allows
    /// `task.send` on `skill:ask`, [`Scope::Project`] when it allows another
    /// task operation there, [`Scope::Public`] otherwise or without a grant.
    ///
    /// Whether the grant is valid, for this agent and now, is the caller's
    /// to check first.
    pub fn for_grant(grant: Option<&Grant>) -> Scope {
        let Some(grant) = grant.filter(|grant| grant.covers_resource(ASK_RESOURCE)) else {
            return Scope::Public;
        };
        if grant.operations.contains(&Operation::TaskSend) {
            Scope::Task(grant.project.clone())
        } else if grant
            .operations
            .iter()
            .any(|op| matches!(op, Operation::TaskRead | Operation::TaskCancel))
        {
            Scope::Project(grant.project.clone())
        } else {
            Scope::Public
        }
    }
}

impl Visibility {
    /// Whether what has this visibility shows in `scope`: `Public` in every
    /// scope, `Project` in its projects' project and task scopes, `Task`
    /// only in its projects' task scopes, `Private` in none.
    pub fn visible_in(&self, scope: &Scope) -> bool {
        match (self, scope) {
            (Visibility::Public, _) => true,
            (Visibility::Project(projects), Scope::Project(project) | Scope::Task(project))
            | (Visibility::Task(projects), Scope::Task(project)) => projects.contains(project),
            _ => false,
        }
    }
}
