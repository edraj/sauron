// Whether the current app serves its aggregates from the rollup tables —
// written by RollupChip (the one component that polls /rollups/status), read
// by pages to decide whether sketch-derived figures carry the ≈ mark. Plain
// module $state: one producer, many readers, reset on app switch.
export const rollupState = $state({ ready: false });
