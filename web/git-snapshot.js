// Find the unresolved advertised tips for background transport scheduling.
// Snapshot application and conflict checks run in Rust/WASM.
export function latestGitTips(project) {
  const announcements = Object.values(project.git_tips ?? {});
  return [...new Set(announcements.map(tip => tip.commit))].filter(tip =>
    /^(?:[a-f0-9]{40}|[a-f0-9]{64})$/.test(tip) &&
    !announcements.some(other => other.commit !== tip && other.includes?.includes(tip)));
}
