/**
 * Guards against drift: bridge/bridge.py is the source of truth, and the page
 * carries an identical copy of it (the app is a single file, so it cannot load
 * the script from disk).
 *
 *   node scripts/check-bridge.mjs
 */
import fs from "node:fs";
import path from "node:path";
import vm from "node:vm";
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

/*
 * 第二道检查：页面运行时把这段文本当模板字面量求值，所以 \x00、\n 这类转义
 * 会被 JS 解释掉，文件字符相等并不代表页面上跑的就是同一段 Python。
 * 历史上 b"\x00\x00" 就这样变成过两个真正的 NUL 字节，页面上复制/下载的
 * bridge.py 直接语法错误。这里按运行时语义再比对一次。
 */
if (embedded.includes("`") || embedded.includes("${")) {
  console.error("bridge: embedded copy contains a backtick or ${ — it cannot be embedded in a template literal");
  process.exit(1);
}
const evaluated = vm.runInNewContext("`" + embedded + "`");
const norm = (s) => s.replace(/\r\n/g, "\n");
if (evaluated.includes("\u0000")) {
  console.error("bridge: evaluated BRIDGE_SCRIPT contains NUL bytes (escape sequence was interpreted)");
  process.exit(1);
}
if (norm(evaluated) !== norm(py)) {
  console.error("bridge: BRIDGE_SCRIPT evaluates to something different from bridge/bridge.py");
  console.error("  the page would copy/download a corrupted script; avoid escapes like \\x00 in bridge.py");
  process.exit(1);
}
console.log(`bridge: evaluated copy matches too (${evaluated.length} chars, no NUL)`);
