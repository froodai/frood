// The console in a real browser against a real server with the demo knowledge (serve.sh).
// The tests run in order and share the server: each leaves the data the next one expects.

import { expect, test, type Page } from "@playwright/test";

/** The development sign-in of the throwaway server, with every permission the console uses. */
async function signIn(page: Page, path = "/admin/") {
  await page.goto(path);
  await page.getByPlaceholder("you@example.com").fill("demo@example.com");
  await page.getByRole("button", { name: "Sign in" }).click();
  for (const scope of ["knowledge:decide", "journal:read", "admin"]) {
    const box = page.getByRole("checkbox", { name: scope });
    if (!(await box.isChecked())) await box.check();
  }
  await page.getByRole("button", { name: "Allow" }).click();
  // Back in the console, at the page asked for.
  await page.waitForURL((url) => url.pathname.startsWith(path === "/admin/" ? "/admin/now" : path) && !url.search.includes("code="));
  await expect(page.getByRole("banner")).toBeVisible();
}

test.describe.configure({ mode: "serial" });

test("Now shows deadlines, decisions and changes, by project", async ({ page }) => {
  await signIn(page);
  await expect(page).toHaveURL(/\/admin\/now$/);
  await expect(page.getByRole("heading", { name: "Now", level: 1 })).toBeVisible();
  const deadlines = page.getByRole("region", { name: "Deadlines" });
  await expect(deadlines.getByRole("link", { name: /overdue\s*Expense report for October/ })).toBeVisible();
  // Older knowledge without a title: its first clause, and the rest under it.
  const decisions = page.getByRole("region", { name: "Decisions in force" });
  await expect(decisions.getByRole("link", { name: /^Payments go through one provider\s*no billing of our own/ })).toBeVisible();
  await page.getByRole("button", { name: "Website" }).click();
  await expect(deadlines.getByText("Copy for the landing page")).toBeVisible();
  await expect(deadlines.getByText("Security review of Atlas")).toHaveCount(0);
});

test("a decision leaves the Inbox at once and comes back with Undo", async ({ page }) => {
  await signIn(page, "/admin/inbox");
  const item = page.getByRole("button", { name: /Maria is responsible for the payments module/ });
  await item.click();
  await expect(page.getByRole("main").getByRole("heading", { level: 2 })).toHaveText("Maria is responsible for the payments module");
  await page.keyboard.press("a");
  await expect(item).toHaveCount(0);
  await page.getByRole("button", { name: "Undo" }).click();
  await expect(page.getByText("Undone")).toBeVisible();
  await expect(item).toBeVisible();
});

test("an item reads as a title with its kind, and opens the original", async ({ page }) => {
  await signIn(page, "/admin/inbox");
  const row = page.getByRole("button", { name: /^ATL-142: a finished session loads for 22 minutes/ });
  await expect(row.getByText("Fact")).toBeVisible();
  await row.click();
  const detail = page.getByRole("main").getByRole("complementary");
  await expect(detail.getByRole("heading", { level: 2 })).toHaveText("ATL-142: a finished session loads for 22 minutes");
  await expect(detail.getByText(/the proposed fix is to attach with session\/resume/)).toBeVisible();
  await expect(detail.getByRole("link", { name: /ATL-142: Task view keeps loading/ })).toHaveAttribute("href", "https://tracker.example/issue/ATL-142");
});

test("several are decided in one go", async ({ page }) => {
  await signIn(page, "/admin/inbox");
  await page.getByRole("checkbox", { name: /Migration to PostgreSQL 17: done/ }).click();
  await page.getByRole("checkbox", { name: /Atlas releases every second Tuesday/ }).click();
  await expect(page.getByText("2 selected")).toBeVisible();
  await page.getByRole("button", { name: "Keep current" }).first().click();
  await expect(page.getByText("Kept the current ones: 2")).toBeVisible();
  await expect(page.getByRole("checkbox", { name: /Migration to PostgreSQL 17: done/ })).toHaveCount(0);
});

test("knowledge shows what changed and since when", async ({ page }) => {
  await signIn(page, "/admin/knowledge");
  await page.getByLabel("Search memory").fill("staging");
  await page.getByRole("button", { name: /Staging of Atlas is at staging\.atlas\.example/ }).click();
  const history = page.getByRole("main");
  await expect(history.getByRole("listitem").filter({ hasText: "stage.atlas.example" })).toHaveCount(1);
  await expect(history.getByText(/in force/)).toBeVisible();
});

test("the palette finds knowledge and sections", async ({ page }) => {
  await signIn(page);
  await page.keyboard.press("ControlOrMeta+k");
  await page.getByPlaceholder(/A command, a section/).fill("security");
  await page.getByRole("option", { name: /Security review of Atlas/ }).click();
  await expect(page).toHaveURL(/\/admin\/knowledge\/[0-9a-f-]+/);
});

test("other languages, right to left for Arabic", async ({ page }) => {
  await signIn(page);
  await page.getByRole("button", { name: /demo@example.com/ }).click();
  await page.getByRole("menuitem", { name: "Language" }).click();
  await page.getByRole("menuitem", { name: "Русский" }).click();
  await expect(page.getByRole("heading", { name: "Сейчас", level: 1 })).toBeVisible();
  await page.evaluate(() => localStorage.setItem("frood.lang", "ar"));
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("dir", "rtl");
  await expect(page.getByRole("heading", { name: "الآن", level: 1 })).toBeVisible();
  await page.evaluate(() => localStorage.setItem("frood.lang", "en"));
});

test("on a phone: bottom navigation and details in a sheet", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await signIn(page, "/admin/inbox");
  const nav = page.getByRole("navigation", { name: "Sections" }).last();
  await expect(nav.getByRole("link", { name: "Inbox" })).toBeVisible();
  await page.getByRole("button", { name: /Atlas release on/ }).click();
  await expect(page.getByRole("dialog").getByText("Proposed")).toBeVisible();
});

test("a note written in the console, and taken back", async ({ page }) => {
  await signIn(page, "/admin/sources");
  await page.getByLabel(/Write a note/).fill("The design review moves to Thursday");
  await page.getByRole("button", { name: "Save note" }).click();
  await expect(page.getByText("Saved; processing")).toBeVisible();
  const row = page.getByRole("button", { name: /The design review moves to Thursday/ });
  await row.click();
  const detail = page.getByRole("dialog");
  await expect(detail.getByText("What came of it")).toBeVisible();
  page.once("dialog", (d) => d.accept());
  await detail.getByRole("button", { name: "Take the note back" }).click();
  await expect(page.getByText(/Taken back/).first()).toBeVisible();
});

test("automations show each job with its schedule and runs", async ({ page }) => {
  await signIn(page, "/admin/automations");
  await expect(page.getByText("Digest").first()).toBeVisible();
  await expect(page.getByText(/weekdays at 08:30/)).toBeVisible();
  await page.getByRole("button", { name: /Run now/ }).click();
  await expect(page.getByText("Started; it shows up in Automations.")).toBeVisible();
});

test("logs read as lines, filtered by level", async ({ page }) => {
  await signIn(page, "/admin/system");
  await expect(page.getByText("[object Object]")).toHaveCount(0);
  await expect(page.getByText(/dispatcher started/)).toBeVisible();
  await page.getByRole("button", { name: "Errors" }).click();
  await expect(page.getByText(/dispatcher started/)).toHaveCount(0);
});
