/**
 * Browser-only replacement for the Node fallback embedded in Graphviz's
 * Emscripten bundle. Runtime environment guards keep this export unreachable.
 */
export function createRequire(): never {
  throw new Error("Node module loading is unavailable in the browser");
}
