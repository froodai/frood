import { describe, expect, it } from "vitest";
import { titled } from "./situations";

describe("titled", () => {
  it("keeps a given title and puts the text under it", () => {
    expect(titled(" Релиз 2.4 — 14 ноября ", "Оля переносит релиз на 14 ноября")).toEqual({
      title: "Релиз 2.4 — 14 ноября",
      description: "Оля переносит релиз на 14 ноября",
    });
    expect(titled("Short", "Short")).toEqual({ title: "Short", description: null });
  });

  it("takes the first clause of older knowledge, cut at a word", () => {
    const text =
      "AIRC-1808 (Bug, Chat): a FINISHED session with a long history (1987 records) shows only the projected tail and keeps loading for about 22 minutes; the proposed fix is to attach with session/resume.";
    const { title, description } = titled(null, text);
    expect(title).toBe("AIRC-1808 (Bug, Chat): a FINISHED session with a long history (1987 records)…");
    expect(title.length).toBeLessThanOrEqual(81);
    expect(description).toBe(
      "…shows only the projected tail and keeps loading for about 22 minutes; the proposed fix is to attach with session/resume.",
    );
    expect(titled(null, "Release on Nov 14. Olya owns it.")).toEqual({ title: "Release on Nov 14", description: "Olya owns it." });
    expect(titled(null, "Payments go through one provider; no billing of our own")).toEqual({
      title: "Payments go through one provider",
      description: "no billing of our own",
    });
    expect(titled(null, "Does the first release support single sign-on?")).toEqual({
      title: "Does the first release support single sign-on?",
      description: null,
    });
    expect(titled(null, "Docs freeze")).toEqual({ title: "Docs freeze", description: null });
  });
});
