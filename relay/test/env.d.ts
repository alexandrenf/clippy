import type { Env as RelayEnv } from "../src/types";

declare global {
  namespace Cloudflare {
    interface Env extends RelayEnv {
      TEST_MIGRATIONS: import("cloudflare:test").D1Migration[];
    }
  }
}

export {};
