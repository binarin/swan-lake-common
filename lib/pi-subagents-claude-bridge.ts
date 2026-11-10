// Load pi-claude-bridge in every pi-subagents child session.
//
// The bridge records each session's system prompt from its own hooks and
// refuses requests whose prompt it has not seen. Foreground children inherit
// the bridge's provider but never load its extension, and background children
// do not see extensions passed as `--extension` flags, so neither works by
// default. Registering the bridge as a required child extension loads it in
// both. Nix substitutes the store paths below.
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { registerRequiredChildExtensions } from "@piSubagents@/src/api/required-child-extensions.js";

const bridgeEntry = "@piClaudeBridge@/claude-bridge/index.ts";

export default function (pi: ExtensionAPI) {
  let registration: { dispose(): void } | undefined;

  pi.on("session_start", (_event, ctx) => {
    registration?.dispose();
    registration = registerRequiredChildExtensions({
      sessionId: ctx.sessionManager.getSessionId(),
      extensions: [{ id: "claude-bridge", path: bridgeEntry }],
    });
  });

  pi.on("session_shutdown", () => {
    registration?.dispose();
    registration = undefined;
  });
}
