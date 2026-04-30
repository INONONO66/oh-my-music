import { Type } from "@mariozechner/pi-ai";
import { defineTool } from "@mariozechner/pi-coding-agent";

import type { EngineClient } from "../../engine-client";
import { clamp } from "../safety";

export function setEnergyTool(engineClient: EngineClient) {
  return defineTool({
    name: "set_energy",
    label: "Set Energy",
    description: "Adjust the overall musical energy of the current mix.",
    parameters: Type.Object({
      targetEnergy: Type.Number({ minimum: 0, maximum: 1 }),
      rampMs: Type.Number({ minimum: 500, maximum: 5000 }),
      reason: Type.String(),
    }),
    executionMode: "sequential",
    execute: async (_toolCallId, params) => {
      const targetEnergy = clamp(params.targetEnergy, 0, 1);
      await engineClient.sendParamBatch({ type: "set_energy", targetEnergy, rampMs: params.rampMs, reason: params.reason });
      return { content: [{ type: "text", text: `Energy set to ${targetEnergy}` }], details: { targetEnergy } };
    },
  });
}

export function setMoodTool(engineClient: EngineClient) {
  return defineTool({
    name: "set_mood",
    label: "Set Mood",
    description: "Apply a high-level musical mood preset to the mix.",
    parameters: Type.Object({
      mood: Type.Union([
        Type.Literal("calm"),
        Type.Literal("focus"),
        Type.Literal("energetic"),
        Type.Literal("dark"),
        Type.Literal("bright"),
        Type.Literal("dreamy"),
        Type.Literal("minimal"),
      ]),
      intensity: Type.Number({ minimum: 0, maximum: 1 }),
      rampMs: Type.Number({ minimum: 500, maximum: 5000 }),
    }),
    executionMode: "sequential",
    execute: async (_toolCallId, params) => {
      await engineClient.sendParamBatch({ type: "set_mood", ...params });
      return { content: [{ type: "text", text: `Mood set to ${params.mood}` }], details: params };
    },
  });
}
