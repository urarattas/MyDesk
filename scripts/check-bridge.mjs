/**
 * Guards against drift: bridge/bridge.py is the source of truth, and the page
 * carries an identical copy of it (the app is a single file, so it cannot load
 * the script from disk).
 *
 *   node scripts/check-bridge.mjs
 */
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const html = fs.readFileSync(path.join(ROOT, "MyDesk.html"), "utf8");
const py = fs.readFileSync(path.join(ROOT, "bridge", "bridge.py"), "utf8");

const marker = "const BRIDGE_SCRIPT = `";
const start = html.indexOf(marker);
if (start < 0) {
  console.error("bridge: cannot find BRIDGE_SCRIPT in MyDesk.html");
  process.exit(1);
}
const from = start + marker.length;
const to = html.indexOf("`;", from);
if (to < 0) {
  console.error("bridge: unterminated BRIDGE_SCRIPT literal");
  process.exit(1);
}

const embedded = html.slice(from, to);
if (embedded !== py) {
  console.error("bridge: bridge/bridge.py and the copy embedded in MyDesk.html differ");
  console.error(`  embedded: ${embedded.length} bytes`);
  console.error(`  file:     ${py.length} bytes`);
  process.exit(1);
}
console.log(`bridge: embedded copy matches bridge/bridge.py (${py.length} bytes)`);
