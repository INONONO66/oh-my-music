import { createMusicAgent } from "./agent/decision";

const agent = await createMusicAgent();
console.log(`oh-my-music agent ready: ${agent.sessionId}`);
