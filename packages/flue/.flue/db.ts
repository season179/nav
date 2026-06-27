import type { PersistenceAdapter } from "@flue/runtime/adapter";
import { sqlite } from "@flue/runtime/node";

// Auto-discovered by the Flue CLI at .flue/db.ts.
const store: PersistenceAdapter = sqlite("./data/flue.db");

export default store;
