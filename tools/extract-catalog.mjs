// Dump the original's built-in extension catalog (data only: ids, versions,
// modes, preconditions, prompt templates, report schemas, settings) to JSON
// so the Rust port can embed it. Usage: node tools/extract-catalog.mjs > src/schema/catalog.json
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
const repo = join(dirname(fileURLToPath(import.meta.url)), "..");
const dist = join(repo, "oracle", "node_modules", "@skill-map", "cli", "dist");
const { builtInPlugins } = await import(join(dist, "chunk-VQPJGTJU.js"));
const KEEP = ["id", "pluginId", "kind", "version", "description", "stability", "defaultEnabled", "mode", "phase",
  "precondition", "promptTemplate", "reportSchema", "probNodeless", "probExpectedDurationSeconds", "defaultPriority",
  "writes", "io", "settings", "emitsFindingTypes", "findingTypes", "summarizer", "tagger"];
const out = [];
for (const plugin of builtInPlugins) {
  for (const ext of plugin.extensions) {
    const row = { plugin: plugin.id ?? ext.pluginId };
    for (const [k, v] of Object.entries(ext)) {
      if (typeof v === "function") { row[`has_${k}`] = true; continue; }
      if (k === "ui" || k === "kinds" || k === "resolution" || k === "reservedNames") { row[k] = v; continue; }
      row[k] = v;
    }
    out.push(row);
  }
}
process.stdout.write(JSON.stringify(out, null, 1) + "\n");
