// Chamberlain framework globals injected into the trigger JS runtime.
// Triggers (`triggers/*/index.ts`) interact with the Rust side through the
// namespace exposed here.
//
// Every side effect below is scoped by the trigger's own manifest.json. The
// runtime has no filesystem, no network and no process access other than these
// ops, so the declaration is the whole permission surface.
//
// Currently exposed:
//   chamberlain.getSecret(name: string): Promise<string | null>
//     -- Read a named secret from the OS credential manager. null if not set,
//        and also null if `name` is not listed in the manifest's
//        `requiredSecrets` (the denial is recorded as `[denied]`).
//        `anthropic_api_key` is never handed to triggers; use ai.complete.
//   chamberlain.ai.complete(opts): Promise<string>
//     opts: { prompt: string, system?: string, model?: string }
//     -- Call the Anthropic Messages API using the anthropic_api_key secret.
//        Rejects if the key is not set. Default model is server-side.
//        Every call is recorded in the activity history as `[ai]`.
//   chamberlain.http.fetch(url, opts?): Promise<{ status: number, body: string }>
//     opts: { method?: string, headers?: Record<string,string>, body?: string }
//     -- Generic HTTP client. Response body is returned as raw text; parse JSON
//        on the caller side. rustyscript's runtime has no built-in `fetch`, so
//        this op fills that gap for triggers that need external HTTP.
//        Rejects unless the host is listed in the manifest's `allowedHosts`.
//        https only (plaintext is allowed to loopback addresses); redirects are
//        followed but every hop is checked against the same declaration.

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
