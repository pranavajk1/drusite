// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Who may do what. Checked on every request; Postgres row-level security is the second line.

// Viewers read.
@id("viewers-read-workspace")
permit (
    principal,
    action == Drusite::Action::"read_workspace",
    resource is Drusite::Workspace
)
when { principal in resource.viewers };

@id("viewers-read-documents")
permit (
    principal,
    action == Drusite::Action::"read_document",
    resource is Drusite::Document
)
when { principal in resource.workspace.viewers };

// Editors write.
@id("editors-create-documents")
permit (
    principal,
    action == Drusite::Action::"create_document",
    resource is Drusite::Workspace
)
when { principal in resource.editors };

@id("editors-write-documents")
permit (
    principal,
    action in [Drusite::Action::"write_document", Drusite::Action::"delete_document"],
    resource is Drusite::Document
)
when { principal in resource.workspace.editors };

// Editors publish pages to the web (viewers can see whether a page is published).
@id("editors-publish-documents")
permit (
    principal,
    action == Drusite::Action::"publish_document",
    resource is Drusite::Document
)
when { principal in resource.workspace.editors };

// An agent publishes only if its person allowed that when connecting it. Off unless they did.
@id("agents-publish-only-when-allowed")
forbid (
    principal,
    action == Drusite::Action::"publish_document",
    resource
)
when { context.via_agent && !context.agent_may_publish };

// Owners manage the workspace.
@id("owners-manage")
permit (
    principal,
    action in [Drusite::Action::"manage_members", Drusite::Action::"delete_workspace"],
    resource is Drusite::Workspace
)
when { principal in resource.owners };

// A disabled account can do nothing, whatever else says so.
@id("disabled-accounts")
forbid (principal, action, resource)
when { principal.disabled };
