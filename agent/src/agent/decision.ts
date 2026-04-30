import { createAgentSession, SessionManager } from "@mariozechner/pi-coding-agent";

import { createEngineClient } from "../engine-client";
import { musicTools } from "./extension";

export async function createMusicAgent() {
  const engineClient = createEngineClient();
  const { session } = await createAgentSession({
    customTools: musicTools(engineClient),
    sessionManager: SessionManager.inMemory(),
  });

  return {
    session,
    sessionId: session.sessionId,
  };
}
