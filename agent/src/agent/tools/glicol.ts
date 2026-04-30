import { Type } from "@mariozechner/pi-ai";
import { defineTool } from "@mariozechner/pi-coding-agent";

import type { EngineClient } from "../../engine-client";

export function createPatternTool(engineClient: EngineClient) {
  return defineTool({
    name: "create_pattern",
    label: "Create Glicol Pattern",
    description: "Create or replace the coded sound layer using Glicol DSL.",
    parameters: Type.Object({
      code: Type.String({ maxLength: 4096, description: "Glicol DSL code" }),
      transitionMs: Type.Number({ minimum: 100, maximum: 16000 }),
      reason: Type.String(),
    }),
    executionMode: "sequential",
    execute: async (_toolCallId, params) => {
      await engineClient.loadGlicolCode(params.code, params.transitionMs);
      return { content: [{ type: "text", text: "Loaded Glicol pattern" }], details: { transitionMs: params.transitionMs } };
    },
  });
}
