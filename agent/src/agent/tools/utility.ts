import { Type } from "@mariozechner/pi-ai";
import { defineTool } from "@mariozechner/pi-coding-agent";

import type { EngineClient } from "../../engine-client";

export function emergencyFadeTool(engineClient: EngineClient) {
  return defineTool({
    name: "emergency_fade",
    label: "Emergency Fade",
    description: "Immediately fade the audio output to safety.",
    parameters: Type.Object({ reason: Type.String() }),
    executionMode: "sequential",
    execute: async (_toolCallId, params) => {
      await engineClient.sendParamBatch({ type: "emergency_fade", fadeMs: 100, reason: params.reason });
      return { content: [{ type: "text", text: "Emergency fade applied" }], details: params };
    },
  });
}

export function resetMixTool(engineClient: EngineClient) {
  return defineTool({
    name: "reset_mix",
    label: "Reset Mix",
    description: "Reset the mix to a safe default state.",
    parameters: Type.Object({ rampMs: Type.Number({ minimum: 100, maximum: 3000 }) }),
    executionMode: "sequential",
    execute: async (_toolCallId, params) => {
      await engineClient.sendParamBatch({ type: "reset_mix", rampMs: params.rampMs });
      return { content: [{ type: "text", text: "Mix reset" }], details: params };
    },
  });
}
