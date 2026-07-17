// Chamberlain framework globals injected into the trigger JS runtime.
// Triggers (`triggers/*/index.ts`) interact with the Rust side through the
// namespace exposed here.
//
// Currently exposed:
//   chamberlain.getSecret(name: string): Promise<string | null>
//     -- Read a named secret from the OS credential manager. null if not set.
//   chamberlain.ai.complete(opts): Promise<string>
//     opts: { prompt: string, system?: string, model?: string }
//     -- Call the Anthropic Messages API using the anthropic_api_key secret.
//        Rejects if the key is not set. Default model is server-side.
//   chamberlain.http.fetch(url, opts?): Promise<{ status: number, body: string }>
//     opts: { method?: string, headers?: Record<string,string>, body?: string }
//     -- Generic HTTP client. Response body is returned as raw text; parse JSON
//        on the caller side. rustyscript's runtime has no built-in `fetch`, so
//        this op fills that gap for triggers that need external HTTP.

const { core } = Deno;

globalThis.chamberlain = {
  async getSecret(name) {
    return await core.ops.op_chamberlain_get_secret(name);
  },
  ai: {
    async complete(opts) {
      return await core.ops.op_chamberlain_ai_complete(opts);
    },
  },
  http: {
    async fetch(url, opts) {
      return await core.ops.op_chamberlain_http_fetch(url, opts);
    },
  },
};
