import type { SymbolArtifact } from '../api/artifacts';

/**
 * Which half of a Dart build's readability is covered, and which is missing.
 *
 * **Two artifacts fix two different things, and uploading one is the easy
 * mistake.** `dart_symbols` (the `--split-debug-info` ELF) resolves stack
 * FRAMES through DWARF. It says nothing about a type name — the Flutter SDK
 * reports an exception's class as `error.runtimeType.toString()`, which
 * `--obfuscate` has already renamed on the device, and only the
 * `--save-obfuscation-map` JSON reverses that. Upload just the symbols and you
 * get a perfectly readable stack trace under a headline that still says `xY1`,
 * which reads as a half-broken product rather than a missing file.
 *
 * This states what IS uploaded rather than guessing whether a build was
 * obfuscated. That distinction is what makes it safe to show: a non-obfuscated
 * build legitimately needs no map, and a warning that assumed otherwise would
 * cry wolf on every one of them. "You have frames but not class names for this
 * build" is true either way — it just does not matter for a build that has no
 * renamed names.
 *
 * Computed from the artifact list the page already has. No extra request, and
 * in particular no query over `error_events` to find out which builds are
 * obfuscated — that would be a scan on an admin page to answer a question the
 * uploader can answer better.
 */
export interface DartBuildCoverage {
  /** The shared `debug_id`; the only thing tying the two artifacts together. */
  debugId: string;
  /** `android` / `ios`, from whichever artifact carried one. */
  platform: string | null;
  /** Architectures the uploaded symbol files cover. Empty when none is present. */
  arches: string[];
  hasSymbols: boolean;
  hasObfuscationMap: boolean;
}

const SYMBOLS = 'dart_symbols';
const MAP = 'dart_obfuscation_map';

/**
 * Group every Dart artifact by build id.
 *
 * Artifacts with no `debug_id` are skipped: a Dart artifact is matched on that
 * id alone, so one without it already matches nothing and belongs in the "this
 * upload is broken" conversation, not this one.
 */
export function dartBuildCoverage(artifacts: SymbolArtifact[]): DartBuildCoverage[] {
  const byBuild = new Map<string, DartBuildCoverage>();
  for (const a of artifacts) {
    if (a.kind !== SYMBOLS && a.kind !== MAP) continue;
    if (!a.debug_id) continue;
    const entry = byBuild.get(a.debug_id) ?? {
      debugId: a.debug_id,
      platform: null,
      arches: [],
      hasSymbols: false,
      hasObfuscationMap: false,
    };
    if (a.kind === SYMBOLS) {
      entry.hasSymbols = true;
      // One build emits one symbols file per architecture, all under the same
      // id, so this legitimately accumulates.
      if (a.arch && !entry.arches.includes(a.arch)) entry.arches.push(a.arch);
    } else {
      entry.hasObfuscationMap = true;
    }
    entry.platform ??= a.platform || null;
    byBuild.set(a.debug_id, entry);
  }
  return [...byBuild.values()];
}

/**
 * The builds missing one of the pair, worst first.
 *
 * Complete builds are dropped rather than listed. This renders as a warning,
 * and a warning that also enumerates everything healthy stops being read —
 * the full inventory is the artifact table right below it.
 *
 * Symbols-without-map is ordered first because it is by far the likelier
 * mistake: `--split-debug-info` is the documented flag everyone knows and
 * `--save-obfuscation-map` is the one they have not heard of. The reverse
 * (a map with no symbols) is rare and usually a half-finished upload.
 */
export function dartCoverageGaps(artifacts: SymbolArtifact[]): DartBuildCoverage[] {
  return dartBuildCoverage(artifacts)
    .filter((b) => !b.hasSymbols || !b.hasObfuscationMap)
    .sort((a, b) => Number(b.hasSymbols) - Number(a.hasSymbols));
}

/** What is missing for this build, as a sentence fragment. */
export function coverageGapLabel(b: DartBuildCoverage): string {
  if (b.hasSymbols && !b.hasObfuscationMap) {
    return 'Stack frames resolve. Exception class names do not — upload this build’s obfuscation map.';
  }
  if (!b.hasSymbols && b.hasObfuscationMap) {
    return 'Exception class names resolve. Stack frames do not — upload this build’s symbol files.';
  }
  // Unreachable through `dartCoverageGaps`, which filters complete builds out.
  // Present so the function is total rather than throwing if it is ever called
  // over the unfiltered list.
  return 'Frames and class names both resolve.';
}
