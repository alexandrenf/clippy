import { copyFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";

const projectRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const tauriRoot = join(projectRoot, "src-tauri");
const target = "aarch64-apple-darwin";
const releaseDir = join(tauriRoot, "target", target, "release");

// Clippy intentionally supports Apple Silicon only. Tauri sidecars use a
// target-suffixed source filename and are renamed back inside the app bundle.
execFileSync(
  "cargo",
  ["build", "--release", "--target", target, "--bin", "clippy-mcp"],
  {
    cwd: tauriRoot,
    stdio: "inherit",
    env: {
      ...process.env,
      // The suffixed sidecar does not exist until this bootstrap build ends.
      TAURI_CONFIG: JSON.stringify({ bundle: { externalBin: [] } }),
    },
  },
);

copyFileSync(
  join(releaseDir, "clippy-mcp"),
  join(releaseDir, `clippy-mcp-${target}`),
);
