import { describe, expect, it } from "vitest";
import { en, type Message } from "./en";
import { LANGUAGES } from ".";

const forms = (m: Message) => (typeof m === "string" ? [m] : Object.values(m));
const holes = (text: string) => [...text.matchAll(/\{(\w+)\}/g)].map((h) => h[1]).sort();

describe("catalogs", () => {
  for (const { code, catalog } of LANGUAGES) {
    it(`${code} has every message of English, with the same placeholders`, () => {
      expect(Object.keys(catalog).sort()).toEqual(Object.keys(en).sort());
      for (const [key, source] of Object.entries(en)) {
        const message = catalog[key as keyof typeof en]!;
        expect(typeof message === "string", key).toBe(typeof source === "string");
        if (typeof message !== "string") expect(message.other, key).toBeTruthy();
        // A plural form may drop {n} ("Accepted"), never add another placeholder.
        const allowed = new Set(forms(source).flatMap(holes));
        for (const text of forms(message)) {
          for (const hole of holes(text)) expect(allowed.has(hole), `${code} ${key}: {${hole}}`).toBe(true);
        }
        if (typeof message === "string") expect(holes(message), `${code} ${key}`).toEqual(holes(source as string));
      }
    });
  }
});
