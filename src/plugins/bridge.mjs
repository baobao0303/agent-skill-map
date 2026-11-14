// Runtime bridge for project-local plugins: agent-skill imports the plugin's
// JavaScript extension modules here (they are ES modules, so only a JS
// runtime can run them) and drives their callbacks. The kernel logic
// (manifest validation, payload validation, confidence folding, messages)
// stays in Rust; this script only imports and calls.
//
// Protocol: one JSON request per stdin line, one JSON response per stdout
// line. Plugin console output is routed to stderr so it can't corrupt it.
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";

const out = process.stdout.write.bind(process.stdout);
for (const k of ["log", "info", "debug"]) console[k] = (...a) => console.error(...a);

const modules = new Map(); // key -> exported default

function describe(err) {
  return err instanceof Error ? err.message : String(err);
}

function manifestView(exported) {
  const view = {};
  for (const [k, v] of Object.entries(exported)) {
    if (typeof v === "function" || k === "pluginId") continue;
    view[k] = v;
  }
  return JSON.parse(JSON.stringify(view));
}

async function load(req) {
  const results = [];
  for (const ext of req.extensions) {
    try {
      const mod = await import(pathToFileURL(ext.file).href);
      const exported = mod && typeof mod === "object" && "default" in mod ? mod.default : mod;
      if (!exported || typeof exported !== "object") {
        results.push({ key: ext.key, ok: false, status: "load-error", error: "missing-kind" });
        continue;
      }
      modules.set(ext.key, exported);
      results.push({
        key: ext.key,
        ok: true,
        keys: Object.keys(exported),
        pluginId: typeof exported.pluginId === "string" ? exported.pluginId : null,
        manifest: manifestView(exported),
      });
    } catch (err) {
      results.push({ key: ext.key, ok: false, status: "load-error", error: describe(err) });
    }
  }
  return { results };
}

function declaredRefs(exported) {
  const refs = new Map();
  const ui = exported.ui;
  if (!ui || typeof ui !== "object") return refs;
  for (const [id, value] of Object.entries(ui)) {
    if (value && typeof value === "object" && typeof value.slot === "string") refs.set(value, id);
  }
  return refs;
}

async function evaluate(req) {
  const exported = modules.get(req.key);
  const nodes = req.nodes;
  const links = req.links;
  const refs = declaredRefs(exported);
  const contributions = [];
  const adjustments = [];
  const ctx = {
    nodes,
    links,
    settings: req.settings ?? {},
    orphanSidecars: req.orphanSidecars ?? [],
    cwd: req.cwd,
    log: {
      debug: (...a) => console.error(...a),
      info: (...a) => console.error(...a),
      warn: (...a) => console.error(...a),
      error: (...a) => console.error(...a),
    },
    emitContribution(nodePath, ref, payload) {
      const id = ref !== null && typeof ref === "object" ? refs.get(ref) : undefined;
      contributions.push({ nodePath, id: id ?? null, payload: payload === undefined ? null : payload });
    },
  };
  if (exported.phase === "score") {
    ctx.adjustConfidence = (link, op) => {
      const index = links.indexOf(link);
      if (index >= 0) adjustments.push({ index, op });
    };
  }
  try {
    const issues = (await exported.evaluate?.(ctx)) ?? [];
    return { issues: Array.isArray(issues) ? issues : [], contributions, adjustments };
  } catch (err) {
    return { error: describe(err), issues: [], contributions, adjustments };
  }
}

const rl = createInterface({ input: process.stdin, crlfDelay: Infinity });
for await (const line of rl) {
  if (!line.trim()) continue;
  let res;
  try {
    const req = JSON.parse(line);
    res = req.op === "load" ? await load(req) : await evaluate(req);
  } catch (err) {
    res = { error: describe(err) };
  }
  out(JSON.stringify(res) + "\n");
}
