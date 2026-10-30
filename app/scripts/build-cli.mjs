// Build the `drovr` CLI and place it where Tauri expects its sidecar binary
// (src-tauri/binaries/drovr-<target-triple>). Agents' MCP configs point at it.
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const profile = process.argv[2] === "release" ? "release" : "debug";
const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const triple = (process.env.TAURI_ENV_TARGET_TRIPLE ||
  /^host: (\S+)/m.exec(execFileSync("rustc", ["-vV"]).toString())[1]).trim();
const host = /^host: (\S+)/m.exec(execFileSync("rustc", ["-vV"]).toString())[1];
const cross = triple !== host ? ["--target", triple] : [];
const args = ["build", "-p", "drovr-cli", ...(profile === "release" ? ["--release"] : []), ...cross];
execFileSync("cargo", args, { cwd: root, stdio: "inherit" });
const ext = process.platform === "win32" ? ".exe" : "";
const dest = join(root, "app", "src-tauri", "binaries");
mkdirSync(dest, { recursive: true });
copyFileSync(join(root, "target", ...(cross.length ? [triple] : []), profile, `drovr${ext}`), join(dest, `drovr-${triple}${ext}`));
console.log(`drovr CLI → src-tauri/binaries/drovr-${triple}${ext}`);
