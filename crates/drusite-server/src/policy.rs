// SPDX-License-Identifier: AGPL-3.0-or-later

//! Authorization: one Cedar policy module, checked on every request.
//!
//! The policies and their schema are built into the binary (`policy/`), validated strictly
//! against the schema when the server starts, and evaluated against entities made from what
//! the server knows about the request: who the user is, whether the account is disabled, and
//! their role in the workspace the request touches. Postgres row-level security is the second
//! line behind it.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use cedar_policy::{
    Authorizer, Context, Decision, Entities, Entity, EntityId, EntityTypeName, EntityUid,
    PolicySet, Request, RestrictedExpression, Schema, ValidationMode, Validator,
};
use drusite_model::{UserId, WorkspaceId};
use uuid::Uuid;

const SCHEMA: &str = include_str!("../policy/drusite.cedarschema");
const POLICIES: &str = include_str!("../policy/drusite.cedar");

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("the Cedar schema doesn't parse: {0}")]
    Schema(String),
    #[error("the Cedar policies don't parse: {0}")]
    Parse(String),
    #[error("the Cedar policies don't validate against the schema: {0}")]
    Validate(String),
}

/// A user's role in a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Owner,
    Editor,
    Viewer,
}

impl Role {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Editor => "editor",
            Self::Viewer => "viewer",
        }
    }

    /// The role one step down, which this one includes.
    const fn below(self) -> Option<Self> {
        match self {
            Self::Owner => Some(Self::Editor),
            Self::Editor => Some(Self::Viewer),
            Self::Viewer => None,
        }
    }
}

impl FromStr for Role {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "owner" => Ok(Self::Owner),
            "editor" => Ok(Self::Editor),
            "viewer" => Ok(Self::Viewer),
            other => Err(format!("unknown role {other:?}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    ReadWorkspace,
    CreateDocument,
    ManageMembers,
    DeleteWorkspace,
    ReadDocument,
    WriteDocument,
    DeleteDocument,
}

impl Action {
    pub const ALL: [Self; 7] = [
        Self::ReadWorkspace,
        Self::CreateDocument,
        Self::ManageMembers,
        Self::DeleteWorkspace,
        Self::ReadDocument,
        Self::WriteDocument,
        Self::DeleteDocument,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::ReadWorkspace => "read_workspace",
            Self::CreateDocument => "create_document",
            Self::ManageMembers => "manage_members",
            Self::DeleteWorkspace => "delete_workspace",
            Self::ReadDocument => "read_document",
            Self::WriteDocument => "write_document",
            Self::DeleteDocument => "delete_document",
        }
    }

    /// Whether the action is on a document rather than a whole workspace.
    pub const fn on_document(self) -> bool {
        matches!(
            self,
            Self::ReadDocument | Self::WriteDocument | Self::DeleteDocument
        )
    }
}

/// Who is asking, as far as the policy needs to know.
#[derive(Debug, Clone, Copy)]
pub struct Principal {
    pub user: UserId,
    pub disabled: bool,
    /// The user's role in the workspace the request is about, if any.
    pub role: Option<Role>,
}

/// What the request is about.
#[derive(Debug, Clone, Copy)]
pub enum Resource {
    Workspace(WorkspaceId),
    /// A page, or the workspace document.
    Document {
        workspace: WorkspaceId,
        document: Uuid,
    },
}

impl Resource {
    const fn workspace(&self) -> WorkspaceId {
        match self {
            Self::Workspace(ws) | Self::Document { workspace: ws, .. } => *ws,
        }
    }
}

#[derive(Debug)]
pub struct Policy {
    schema: Schema,
    policies: PolicySet,
    authorizer: Authorizer,
}

fn uid(type_name: &str, id: &str) -> EntityUid {
    let ty = EntityTypeName::from_str(type_name).expect("entity type names are constants");
    EntityUid::from_type_name_and_id(ty, EntityId::new(id))
}

fn role_uid(workspace: WorkspaceId, role: Role) -> EntityUid {
    uid("Drusite::Role", &format!("{workspace}/{}", role.as_str()))
}

impl Policy {
    /// Parses the built-in schema and policies and validates them strictly.
    pub fn load() -> Result<Self, PolicyError> {
        let (schema, warnings) =
            Schema::from_cedarschema_str(SCHEMA).map_err(|e| PolicyError::Schema(e.to_string()))?;
        for warning in warnings {
            tracing::warn!(%warning, "Cedar schema warning");
        }
        let policies =
            PolicySet::from_str(POLICIES).map_err(|e| PolicyError::Parse(e.to_string()))?;
        let result = Validator::new(schema.clone()).validate(&policies, ValidationMode::Strict);
        if !result.validation_passed_without_warnings() {
            let problems: Vec<String> = result
                .validation_errors()
                .map(ToString::to_string)
                .chain(result.validation_warnings().map(ToString::to_string))
                .collect();
            return Err(PolicyError::Validate(problems.join("; ")));
        }
        Ok(Self {
            schema,
            policies,
            authorizer: Authorizer::new(),
        })
    }

    /// Whether `principal` may do `action` to `resource`. Anything malformed (an action on the
    /// wrong kind of resource) is denied.
    pub fn is_allowed(&self, principal: &Principal, action: Action, resource: &Resource) -> bool {
        match self.decide(principal, action, resource) {
            Ok(decision) => decision == Decision::Allow,
            Err(error) => {
                tracing::error!(%error, ?action, ?resource, "malformed authorization request");
                debug_assert!(false, "malformed authorization request: {error}");
                false
            }
        }
    }

    fn decide(
        &self,
        principal: &Principal,
        action: Action,
        resource: &Resource,
    ) -> Result<Decision, String> {
        let ws = resource.workspace();
        let mut entities = Vec::new();

        // The workspace's three roles, each inside the one below it.
        for role in [Role::Owner, Role::Editor, Role::Viewer] {
            let parents = role.below().map(|r| role_uid(ws, r)).into_iter().collect();
            entities.push(Entity::new_no_attrs(role_uid(ws, role), parents));
        }

        let user_uid = uid("Drusite::User", &principal.user.to_string());
        let user_parents = principal
            .role
            .map(|r| role_uid(ws, r))
            .into_iter()
            .collect::<HashSet<_>>();
        entities.push(
            Entity::new(
                user_uid.clone(),
                HashMap::from([(
                    "disabled".to_owned(),
                    RestrictedExpression::new_bool(principal.disabled),
                )]),
                user_parents,
            )
            .map_err(|e| e.to_string())?,
        );

        let ws_uid = uid("Drusite::Workspace", &ws.to_string());
        entities.push(
            Entity::new(
                ws_uid.clone(),
                HashMap::from([
                    (
                        "viewers".to_owned(),
                        RestrictedExpression::new_entity_uid(role_uid(ws, Role::Viewer)),
                    ),
                    (
                        "editors".to_owned(),
                        RestrictedExpression::new_entity_uid(role_uid(ws, Role::Editor)),
                    ),
                    (
                        "owners".to_owned(),
                        RestrictedExpression::new_entity_uid(role_uid(ws, Role::Owner)),
                    ),
                ]),
                HashSet::new(),
            )
            .map_err(|e| e.to_string())?,
        );

        let resource_uid = match resource {
            Resource::Workspace(_) => ws_uid,
            Resource::Document { document, .. } => {
                let doc_uid = uid("Drusite::Document", &document.to_string());
                entities.push(
                    Entity::new(
                        doc_uid.clone(),
                        HashMap::from([(
                            "workspace".to_owned(),
                            RestrictedExpression::new_entity_uid(ws_uid.clone()),
                        )]),
                        HashSet::from([ws_uid]),
                    )
                    .map_err(|e| e.to_string())?,
                );
                doc_uid
            }
        };

        let entities =
            Entities::from_entities(entities, Some(&self.schema)).map_err(|e| e.to_string())?;
        let request = Request::new(
            user_uid,
            uid("Drusite::Action", action.name()),
            resource_uid,
            Context::empty(),
            Some(&self.schema),
        )
        .map_err(|e| e.to_string())?;
        Ok(self
            .authorizer
            .is_authorized(&request, &self.policies, &entities)
            .decision())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(role: Option<Role>, disabled: bool) -> Principal {
        Principal {
            user: UserId::new(),
            disabled,
            role,
        }
    }

    #[test]
    fn the_built_in_policies_validate() {
        Policy::load().unwrap();
    }

    #[test]
    fn each_role_gets_exactly_its_actions() {
        let policy = Policy::load().unwrap();
        let ws = WorkspaceId::new();
        let doc = Resource::Document {
            workspace: ws,
            document: Uuid::now_v7(),
        };
        let workspace = Resource::Workspace(ws);

        let expected = |role: Option<Role>, action: Action| -> bool {
            use Action::*;
            match role {
                None => false,
                Some(Role::Viewer) => matches!(action, ReadWorkspace | ReadDocument),
                Some(Role::Editor) => !matches!(action, ManageMembers | DeleteWorkspace),
                Some(Role::Owner) => true,
            }
        };
        for role in [
            None,
            Some(Role::Viewer),
            Some(Role::Editor),
            Some(Role::Owner),
        ] {
            for action in Action::ALL {
                let resource = if action.on_document() { doc } else { workspace };
                assert_eq!(
                    policy.is_allowed(&principal(role, false), action, &resource),
                    expected(role, action),
                    "{role:?} {action:?}"
                );
                // A disabled account is refused everything.
                assert!(!policy.is_allowed(&principal(role, true), action, &resource));
            }
        }
    }

    #[test]
    fn a_role_in_one_workspace_gives_nothing_in_another() {
        let policy = Policy::load().unwrap();
        let mine = WorkspaceId::new();
        let theirs = WorkspaceId::new();
        // The principal's role is always for the resource's workspace, so the request is
        // modelled as the server would: no role in theirs.
        let me = principal(Some(Role::Owner), false);
        assert!(policy.is_allowed(&me, Action::DeleteWorkspace, &Resource::Workspace(mine)));
        let outsider = Principal { role: None, ..me };
        for action in Action::ALL {
            let resource = if action.on_document() {
                Resource::Document {
                    workspace: theirs,
                    document: Uuid::now_v7(),
                }
            } else {
                Resource::Workspace(theirs)
            };
            assert!(
                !policy.is_allowed(&outsider, action, &resource),
                "{action:?}"
            );
        }
    }
}
