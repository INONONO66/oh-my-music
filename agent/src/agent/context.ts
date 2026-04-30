export type MusicAgentContext = {
  mode: "tui" | "discord";
  energy: number;
  environment: "quiet" | "cafe" | "outdoor" | "speech" | "music" | "noise" | "unknown";
};

export function defaultContext(): MusicAgentContext {
  return { mode: "tui", energy: 0.5, environment: "unknown" };
}
