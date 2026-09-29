// The gutter's lane budget: how many columns draw at full spacing before
// the rest compress into the narrow band (see `fullLanesFor` in graph.ts).
// The workbench measures its pane and sets it; every row renderer reads
// it. A shared session like the row density, so both view modes and the
// elision rows agree without threading a prop through each of them.

import { DEFAULT_FULL_LANES } from "./graph";

export const laneBudget = $state({ full: DEFAULT_FULL_LANES });
