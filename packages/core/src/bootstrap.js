// Chamberlain framework globals injected into the trigger JS runtime.
// Triggers (`triggers/*/index.ts`) interact with the Rust side through the
// namespace exposed here.
//
// Currently exposed:
//   chamberlain.getSecret(name: string): Promise<string | null>
//     -- Read a named secret from the OS credential manager. null if not set.
//
// Future entry points (chamberlain.ai.complete, chamberlain.readAsset, etc.)
// will be added to this same object. See docs/architecture.md for
// "AI types" and "future decisions".

const { core } = Deno;

globalThis.chamberlain = {
  async getSecret(name) {
    return await core.ops.op_chamberlain_get_secret(name);
  },
};
