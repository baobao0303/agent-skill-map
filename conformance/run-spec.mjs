// Run the spec-owned conformance cases with the ORIGINAL runner against
// either implementation: `node conformance/run-spec.mjs <sm|agent-skill> [case-id...]`.
import { readdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const repo = join(dirname(fileURLToPath(import.meta.url)), "..");
const oracle = join(repo, "oracle", "node_modules", "@skill-map");
const { runConformanceCase } = await import(join(oracle, "cli", "dist", "conformance", "index.js"));
const specRoot = join(oracle, "spec");
const casesDir = join(specRoot, "conformance", "cases");
const [which, ...only] = process.argv.slice(2);
const binary = which === "sm" ? join(oracle, "cli", "bin", "sm.js") : join(repo, "conformance", "agent-skill-shim.cjs");
const ids = (only.length ? only : readdirSync(casesDir).map((f) => f.replace(/\.json$/, ""))).sort();
let failed = 0;
for (const id of ids) {
  const r = await runConformanceCase({
    binary,
    specRoot,
    casePath: join(casesDir, `${id}.json`),
    env: { NO_COLOR: "1", SKILL_MAP_TELEMETRY: "0", SM_NO_UPDATE_CHECK: "1" },
  });
  if (!r.passed) failed++;
  const why = r.assertions.filter((a) => !a.ok).map((a) => `${a.type}: ${a.reason}`).join(" | ");
  console.log(`${r.passed ? "PASS" : "FAIL"}  ${id}${why ? "  -- " + why.slice(0, 300) : ""}`);
}
console.log(`\n${ids.length - failed}/${ids.length} passed`);
process.exit(failed ? 1 : 0);
