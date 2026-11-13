// Lets the original conformance runner (which spawns `node <binary> ...`)
// drive the Rust binary: forwards argv, stdio and the exit status.
const { spawnSync } = require("node:child_process");
const path = require("node:path");
const bin = process.env.ASM_BIN || path.join(__dirname, "..", "target", "release", "asm");
const r = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });
process.exit(r.status === null ? 1 : r.status);
