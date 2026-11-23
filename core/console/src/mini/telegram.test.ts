import { describe, expect, it } from "vitest";
import { launchHash, otherMiniApp } from "./telegram";

/** How Telegram's script reads the launch parameters from the hash (urlParseHashParams). */
function parsed(hash: string): Record<string, string> {
  const decode = (v: string) => decodeURIComponent(v.replace(/\+/g, "%20"));
  return Object.fromEntries(
    hash
      .replace(/^#/, "")
      .split("&")
      .map((pair) => pair.split("="))
      .map(([k, v]) => [decode(k), decode(v ?? "")]),
  );
}

describe("the other installation's Mini App", () => {
  const app = {
    initData: "query_id=AAE&user=%7B%22id%22%3A42%7D&auth_date=1&signature=a+b/c&hash=x",
    version: "8.0",
    platform: "ios",
    themeParams: { bg_color: "#ffffff", text_color: "#000000" },
  };

  it("gets Telegram's launch data unchanged, as Telegram's script reads it", () => {
    const params = parsed(launchHash(app));
    expect(params.tgWebAppData).toBe(app.initData);
    expect(params.tgWebAppVersion).toBe("8.0");
    expect(params.tgWebAppPlatform).toBe("ios");
    expect(JSON.parse(params.tgWebAppThemeParams)).toEqual(app.themeParams);
  });

  it("opens at its address with the data in the hash only", () => {
    const url = otherMiniApp("https://work.example.com/admin/m/#old", app);
    expect(url.startsWith("https://work.example.com/admin/m/#tgWebAppData=")).toBe(true);
    expect(new URL(url).search).toBe("");
  });
});
