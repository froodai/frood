// OpenClaw's plugin entry as the plugin uses it. The host provides the module at run time; the
// full `openclaw` package (250 MB) is not needed to build the plugin.
declare module "openclaw/plugin-sdk/plugin-entry" {
  export function definePluginEntry<T extends { id: string; register(api: never): void }>(entry: T): T;
}
