import type { RepoSnapshot } from "$lib/bindings/RepoSnapshot";
import type { RepoPrState } from "$lib/bindings/RepoPrState";
import type { PublishStack } from "$lib/bindings/PublishStack";
import fixture from "../../../../crates/jiji-forge/tests/fixtures/publish-stack.json";

/** The same jjpr-style fixture is verified against Rust discovery and
 * rendered by the desktop tests. JSON matches the actual IPC representation. */
export function publishFixture(): {
  snapshot: RepoSnapshot;
  prs: RepoPrState;
  stacks: PublishStack[];
} {
  return structuredClone(fixture) as unknown as {
    snapshot: RepoSnapshot;
    prs: RepoPrState;
    stacks: PublishStack[];
  };
}
