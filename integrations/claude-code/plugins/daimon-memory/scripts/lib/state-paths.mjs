// Platform-appropriate per-user state directory root, shared by every script that
// persists session state (recall/nudge/precompact). XDG_STATE_HOME always wins so a
// user can redirect state explicitly; on Windows the fallback is %LOCALAPPDATA% (the
// idiomatic per-user state location) instead of a ~/.local/state shadow tree that only
// Git Bash users would ever find.
import { homedir } from "node:os";
import { join } from "node:path";

export function stateBase() {
  if (process.env.XDG_STATE_HOME) return process.env.XDG_STATE_HOME;
  if (process.platform === "win32") {
    return process.env.LOCALAPPDATA || join(homedir(), "AppData", "Local");
  }
  return join(homedir(), ".local", "state");
}
