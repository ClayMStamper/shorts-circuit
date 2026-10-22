// Runs before `vite` in `pnpm dev` (which `tauri dev` starts). When the app crashes, `tauri dev`
// can exit while its Vite server keeps running and holding the dev port, so the next start fails
// with "Port 1420 is already in use". If the process holding the port is this app's own Vite,
// it is an orphan from an earlier run: stop it and carry on. Anything else is left alone and
// reported, since it isn't ours to stop.

import { execFileSync } from "node:child_process";
import net from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";

// Vite's `server.port` (strictPort). SHORTS_DEV_PORT overrides it for testing this script.
const PORT = Number(process.env.SHORTS_DEV_PORT) || 1420;
const appDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/** Whether something accepts connections on the port (Vite may listen on IPv4 or IPv6). */
async function portInUse(port) {
  const tryHost = (host) =>
    new Promise((resolve) => {
      const socket = net.connect({ port, host });
      socket.setTimeout(500);
      socket.once("connect", () => {
        socket.destroy();
        resolve(true);
      });
      socket.once("timeout", () => {
        socket.destroy();
        resolve(false);
      });
      socket.once("error", () => resolve(false));
    });
  const results = await Promise.all([tryHost("127.0.0.1"), tryHost("::1")]);
  return results.some(Boolean);
}

/** Processes listening on the port, with their command lines. */
function listeners(port) {
  try {
    if (process.platform === "win32") {
      const script =
        `Get-NetTCPConnection -LocalPort ${port} -State Listen -ErrorAction SilentlyContinue | ` +
        "Select-Object -ExpandProperty OwningProcess -Unique | ForEach-Object { " +
        '$p = Get-CimInstance Win32_Process -Filter "ProcessId = $_"; ' +
        "[pscustomobject]@{ pid = [int]$_; cmd = [string]$p.CommandLine } } | ConvertTo-Json -Compress";
      const out = execFileSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", script], {
        encoding: "utf8",
        windowsHide: true,
      }).trim();
      if (!out) return [];
      const parsed = JSON.parse(out);
      return Array.isArray(parsed) ? parsed : [parsed];
    }
    const pids = execFileSync("lsof", ["-nP", `-iTCP:${port}`, "-sTCP:LISTEN", "-t"], { encoding: "utf8" })
      .split(/\s+/)
      .filter(Boolean)
      .map(Number);
    return pids.map((pid) => ({
      pid,
      cmd: execFileSync("ps", ["-o", "command=", "-p", String(pid)], { encoding: "utf8" }).trim(),
    }));
  } catch {
    return [];
  }
}

const normalize = (s) => s.replaceAll("\\", "/").toLowerCase();

/** This app's own Vite: a command line that runs vite from this app's node_modules. */
function isOurVite(cmd) {
  const c = normalize(cmd ?? "");
  return c.includes(normalize(path.join(appDir, "node_modules"))) && c.includes("vite");
}

async function waitUntilFree(port, ms) {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    if (!(await portInUse(port))) return true;
    await new Promise((r) => setTimeout(r, 200));
  }
  return false;
}

if (await portInUse(PORT)) {
  const found = listeners(PORT);
  const ours = found.filter((p) => isOurVite(p.cmd));
  const others = found.filter((p) => !isOurVite(p.cmd));
  if (others.length > 0 || ours.length === 0) {
    console.error(`Port ${PORT} is in use by another program, so the dev server can't start:`);
    for (const p of others) console.error(`  PID ${p.pid}: ${p.cmd || "(command line unavailable)"}`);
    if (found.length === 0) console.error("  (couldn't tell which process; check with your OS tools)");
    console.error("Close it, then run `pnpm tauri dev` again.");
    process.exit(1);
  }
  for (const p of ours) {
    console.log(`Stopping a leftover dev server from an earlier run (PID ${p.pid}).`);
    try {
      process.kill(p.pid);
    } catch (e) {
      console.error(`Couldn't stop PID ${p.pid}: ${e.message}`);
      process.exit(1);
    }
  }
  if (!(await waitUntilFree(PORT, 5000))) {
    console.error(`Port ${PORT} is still in use after stopping the leftover dev server.`);
    process.exit(1);
  }
}
