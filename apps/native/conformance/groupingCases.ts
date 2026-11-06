// Synthetic grouping/preferences cases evaluated by the actual current TypeScript helpers.
import type { EnvironmentProject } from "../../../packages/client-runtime/src/state/models.ts";
import type { EnvironmentId, ScopedProjectRef, SidebarProjectGroupingMode } from "../../../packages/contracts/src/index.ts";
import { ProjectGrouping, SidebarProjectGrouping, UiProjectState } from "./runtime.ts";

const project = (environmentId: string, id: string, path: string, extra = {}) => ({
  environmentId, id, title: id, workspaceRoot: path,
  repositoryIdentity: { canonicalKey: "repo", rootPath: "/repo" },
  createdAt: "2026-01-01T00:00:00.000Z", updatedAt: "2026-01-01T00:00:00.000Z",
  pinnedAt: null, archivedAt: null, isScratch: false, ...extra,
}) as unknown as EnvironmentProject;

export function groupingCases() {
  const projects = [
    project("a", "same", "/repo"),
    project("b", "same", "/repo", { title: "Other machine" }),
    project("a", "old", "/copy/", { updatedAt: "2025-01-01T00:00:00.000Z" }),
    project("a", "winner", "/copy", { updatedAt: "2026-02-01T00:00:00.000Z", repositoryIdentity: null, pinnedAt: "2026-02-01T00:00:00.000Z" }),
    project("a", "sub", "/repo/apps/web"),
  ];
  const cases = [];
  for (const mode of ["repository", "repository_path", "separate"] as const) {
    for (const split of [false, true]) {
      const settings = {
        sidebarProjectGroupingMode: mode,
        sidebarProjectGroupingOverrides: split ? { "a:/copy": "separate" as SidebarProjectGroupingMode } : {},
      };
      const groups = ProjectGrouping.buildProjectGroups({ projects, settings, preferredEnvironmentId: "b" as EnvironmentId });
      cases.push({ kind: "grouping", name: `${mode}-${split}`, projects, settings, preferred: "b", scratch: false,
        expected: groups.map((group) => ({ key: group.key, label: group.label,
          representative: { environmentId: group.representative.environmentId, projectId: group.representative.id },
          refs: group.memberProjectRefs,
          members: group.members.map((member) => ({ physical: member.physicalProjectKey, environmentId: member.project.environmentId, projectId: member.project.id })),
          pinned: group.pinned, archived: group.archived,
        })),
      });
    }
  }
  for (const mode of ["repository", "repository_path", "separate"] as const) {
    for (const scratch of [false, true]) {
      const projects = [project("a", "scratch-a", "/repo/apps/a", { isScratch: true }), project("b", "scratch-b", "/repo/apps/b", { isScratch: true })];
      const settings = { sidebarProjectGroupingMode: mode, sidebarProjectGroupingOverrides: {} };
      const groups = SidebarProjectGrouping.buildSidebarProjectSnapshots({ projects, settings, groupScratchProjects: scratch, primaryEnvironmentId: "b" as EnvironmentId, resolveEnvironmentLabel: (id) => id });
      const scope = [ProjectGrouping.deriveLogicalProjectKeyFromSettings(projects[0]!, settings), "missing"];
      const context = { environmentId: "a", projectId: "scratch-a" } as ScopedProjectRef;
      cases.push({ kind: "scope", name: `scratch-${mode}-${scratch}`, projects, settings, preferred: "b", scratch, scope,
        expectedScope: SidebarProjectGrouping.resolveSidebarProjectScopeKeys({ groups, keys: scope, settings, canClearMissingScope: true }),
        expectedLoading: SidebarProjectGrouping.resolveSidebarProjectScopeKeys({ groups, keys: scope, settings, canClearMissingScope: false }),
        context,
        expectedTargets: SidebarProjectGrouping.buildSidebarProjectPickerEntries({ groups, preferredProjectRef: context }).map((entry) => ({ environmentId: entry.targetProject.environmentId, projectId: entry.targetProject.id })),
      });
    }
  }
  const windows = [project("win", "old", "C:/Work/Code/", { repositoryIdentity: { origin: { canonicalKey: "fork", displayName: "My fork" }, rootPath: "C:/Work/Code" } }), project("win", "new", "c:\\work\\code", { updatedAt: "2026-02-01T00:00:00.000Z", repositoryIdentity: null })];
  const settings = { sidebarProjectGroupingMode: "repository" as const, sidebarProjectGroupingOverrides: {} };
  const groups = ProjectGrouping.buildProjectGroups({ projects: windows, settings });
  cases.push({ kind: "grouping", name: "windows-origin-identity-carry", projects: windows, settings, preferred: null, scratch: false,
    expected: groups.map((group) => ({ key: group.key, label: group.label, representative: { environmentId: group.representative.environmentId, projectId: group.representative.id }, refs: group.memberProjectRefs, members: group.members.map((member) => ({ physical: member.physicalProjectKey, environmentId: member.project.environmentId, projectId: member.project.id })), pinned: group.pinned, archived: group.archived })),
  });
  for (const { dragged, target } of [
    { dragged: ["a"], target: ["b"] },
    { dragged: ["a", "b"], target: ["b"] },
    { dragged: ["c"], target: ["b"] },
    { dragged: ["missing"], target: ["b"] },
    { dragged: ["a", "d"], target: ["d"] },
  ]) {
    const current = ["a", "b", "c", "d"];
    const state = { projectExpandedById: {}, projectOrder: [], sidebarProjectScopeKeys: [], threadLastVisitedAtById: {}, threadChangedFilesExpandedById: {}, defaultAdvertisedEndpointKey: null, pullRequestMergeMethod: "merge" as const };
    cases.push({ kind: "reorder", name: `reorder-${dragged.join("-")}`, current, dragged, target,
      expected: UiProjectState.reorderProjects(state, current, dragged, target).projectOrder,
    });
  }
  for (const record of [
    { "legacy-project-expansion-default": false },
    { "legacy-project-cwd:/repo": false, "legacy-project-expansion-default": true },
    { "repo": true, "a:/repo": false, "legacy-project-cwd:/repo": false },
  ]) {
    const keys = ["repo", "a:/repo", "legacy-project-cwd:/repo"];
    cases.push({ kind: "expansion", name: JSON.stringify(record), record, keys,
      expected: UiProjectState.resolveProjectExpanded(record, keys),
    });
  }
  return cases;
}
