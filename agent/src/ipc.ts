export type EngineClient = {
  sendParamBatch(batch: unknown): Promise<void>;
  loadGlicolCode(code: string, transitionMs: number): Promise<void>;
};

export function createEngineClient(): EngineClient {
  return {
    async sendParamBatch(batch) {
      console.log("sendParamBatch", batch);
    },
    async loadGlicolCode(code, transitionMs) {
      console.log("loadGlicolCode", { code, transitionMs });
    },
  };
}
