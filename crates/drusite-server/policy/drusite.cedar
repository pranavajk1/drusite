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
