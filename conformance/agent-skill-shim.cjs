// Lets the original conformance runner (which spawns `node <binary> ...`)
// drive the Rust binary: forwards argv, stdio, signals and the exit status.
// Signals matter: the runner tears `setup.serve` down with SIGTERM/SIGKILL on
// this shim, so the child must die with it or it leaks and holds stdio open.
const { spawn } = require("node:child_process");
const path = require("node:path");
const bin = process.env.AGENT_SKILL_BIN || path.join(__dirname, "..", "target", "release", "agent-skill");
const child = spawn(bin, process.argv.slice(2), { stdio: "inherit" });
for (const sig of ["SIGTERM", "SIGINT", "SIGHUP"]) {
  process.on(sig, () => child.kill(sig));
}
child.on("error", (e) => {
  process.stderr.write(`agent-skill-shim: ${e.message}\n`);
  process.exit(1);
});
child.on("exit", (code, signal) => {
  if (signal) {
    process.removeAllListeners(signal);
    process.kill(process.pid, signal);
  } else {
    process.exit(code === null ? 1 : code);
  }
});
