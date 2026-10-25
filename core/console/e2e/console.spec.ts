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

test("spheres: a sign on each item, a filter, and a move in one click", async ({ page }) => {
  await signIn(page, "/admin/knowledge");
  const filter = page.getByRole("main").locator("section").getByRole("combobox", { name: "Sphere" });
  await filter.click();
  await page.getByRole("option", { name: "Personal" }).click();
  await expect(page).toHaveURL(/sphere=personal/);
  await expect(page.getByRole("button", { name: /Book flights for the holidays/ })).toBeVisible();
  await expect(page.getByRole("button", { name: /Copy for the landing page/ })).toHaveCount(0);

  // Moved by hand: accepted at once.
  await filter.click();
  await page.getByRole("option", { name: "All spheres" }).click();
  await page.getByRole("button", { name: /Copy for the landing page/ }).click();
  const picker = page.getByRole("complementary").getByRole("combobox", { name: "Sphere" });
  await expect(picker).toHaveText("Work · business");
  await picker.click();
  await page.getByRole("option", { name: "Personal" }).click();
  await expect(page.getByText("Moved to “Personal”")).toBeVisible();
  await filter.click();
  await page.getByRole("option", { name: "Personal" }).click();
  await expect(page.getByRole("button", { name: /Copy for the landing page/ })).toBeVisible();

  // Now narrows to a sphere.
  await page.goto("/admin/now");
  await page.getByRole("button", { name: "Personal" }).click();
  const deadlines = page.getByRole("region", { name: "Deadlines" });
  await expect(deadlines.getByText("Book flights for the holidays")).toBeVisible();
  await expect(deadlines.getByText("Security review of Atlas")).toHaveCount(0);

  // In the Inbox: the proposed sphere, changed as the change is accepted.
  await page.goto("/admin/inbox");
  const row = page.getByRole("button", { name: /^Search moves to OpenSearch/ });
  await expect(row.getByText("Work · tech")).toBeVisible();
  await row.click();
  await expect(page.getByRole("complementary").getByRole("heading", { level: 2 })).toHaveText(/^Search moves to OpenSearch/);
  const proposed = page.getByRole("complementary").getByRole("combobox", { name: "Sphere" });
  await proposed.click();
  await page.getByRole("option", { name: "Work · business" }).click();
  await page.getByRole("complementary").getByRole("button", { name: /^Accept/ }).click();
  await expect(row).toHaveCount(0);
  await page.goto("/admin/knowledge?sphere=business");
  await expect(page.getByRole("button", { name: /Search moves to OpenSearch/ })).toBeVisible();
});

test("attachments: a photo and a link on knowledge, and on a change that waits", async ({ page }) => {
  await signIn(page, "/admin/knowledge");
  await page.getByRole("button", { name: /Payments demo for the team/ }).click();
  const detail = page.getByRole("complementary");
  const attachments = detail.getByRole("region", { name: "Attachments" });
  // A tiny PNG, as if chosen in the file dialog.
  const png = Buffer.from(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==",
    "base64",
  );
  await attachments.locator('input[type="file"]').setInputFiles({ name: "board.png", mimeType: "image/png", buffer: png });
  await expect(attachments.getByRole("img")).toBeVisible();
  await attachments.getByLabel("Paste a link").fill("https://docs.example/demo-plan");
  await attachments.getByRole("button", { name: "Add" }).click();
  const link = attachments.getByRole("link", { name: /docs\.example/ });
  await expect(link).toHaveAttribute("href", "https://docs.example/demo-plan");
  // Taken off again.
  await link.hover();
  await attachments.getByRole("button", { name: "Remove" }).last().click();
  await expect(link).toHaveCount(0);
  await expect(attachments.getByRole("img")).toBeVisible();

  // On a change that waits: the row shows a paper clip with the count.
  await page.goto("/admin/inbox");
  const row = page.getByRole("button", { name: /^ATL-142: a finished session loads for 22 minutes/ });
  await row.click();
  // The details switch to the item before anything is typed there.
  await expect(page.getByRole("complementary").getByRole("heading", { level: 2 })).toHaveText(/^ATL-142/);
  const waiting = page.getByRole("complementary").getByRole("region", { name: "Attachments" });
  await waiting.getByLabel("Paste a link").fill("https://tracker.example/issue/ATL-142#screenshot");
  await waiting.getByRole("button", { name: "Add" }).click();
  await expect(waiting.getByRole("link", { name: /tracker\.example/ })).toBeVisible();
  await expect(row.getByLabel("Attachments")).toHaveText("1");
});

test("a source connected from the console: check, tools, connect, disconnect", async ({ page }) => {
  await signIn(page, "/admin/sources");
  await page.getByRole("tab", { name: "Connected" }).click();
  await page.getByRole("button", { name: /Connect a source/ }).click();
  await page.getByRole("button", { name: /Any MCP server/ }).click();
  const form = page.getByRole("dialog");
  await form.getByLabel("Name").fill("tracker");
  await form.getByLabel("Server address").fill("http://127.0.0.1:18099/mcp");
  await form.getByLabel("Token").fill("wrong");
  await form.getByRole("button", { name: "Check" }).click();
  await expect(page.getByText(/refused the token/)).toBeVisible();
  await form.getByLabel("Token").fill("e2e-token");
  await form.getByRole("button", { name: "Check" }).click();
  await expect(page.getByText("Connected: 2 tools")).toBeVisible();
  // Reading tools are chosen; the changing one is not.
  await expect(form.getByRole("checkbox", { name: /search_issues/ })).toBeChecked();
  await expect(form.getByRole("checkbox", { name: /update_issue/ })).not.toBeChecked();
  await form.getByRole("button", { name: "Connect", exact: true }).click();
  await expect(page.getByText("Connected: tracker. The next pass reads it.")).toBeVisible();

  const card = page.getByRole("button", { name: /^tracker/ });
  await card.click();
  await expect(page.getByRole("dialog").getByLabel("Token")).toHaveAttribute("placeholder", /A token is stored/);
  await page.getByRole("dialog").getByRole("button", { name: "Disconnect" }).click();
  await expect(card).toHaveCount(0);
});

test("a source connected by signing in, then its tools chosen", async ({ page }) => {
  await signIn(page, "/admin/sources");
  await page.getByRole("tab", { name: "Connected" }).click();
  await page.getByRole("button", { name: /Connect a source/ }).click();
  await page.getByRole("button", { name: /Any MCP server/ }).click();
  const form = page.getByRole("dialog");
  await form.getByLabel("Name").fill("wiki");
  await form.getByLabel("Server address").fill("http://127.0.0.1:18099/signed/mcp");
  await form.getByRole("button", { name: "Sign in" }).click();
  // The server's page sends the person straight back; the source opens to choose its tools.
  await expect(page.getByText("Signed in: wiki. Choose the tools the agent may call.")).toBeVisible();
  const opened = page.getByRole("dialog");
  await expect(opened.getByText("Connected by signing in; the token renews itself.")).toBeVisible();
  await expect(opened.getByRole("checkbox", { name: /search_issues/ })).toBeChecked();
  await opened.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("Connected: wiki. The next pass reads it.")).toBeVisible();
});

test("jobs from the console: frood.yaml's stay there, others change and come back", async ({ page }) => {
  await signIn(page, "/admin/automations");
  await page.getByRole("button", { name: "Settings: Digest" }).click();
  await expect(page.getByText("This job is set in frood.yaml and changes only there; run frood apply after.")).toBeVisible();
  await page.keyboard.press("Escape");

  // The nightly pass needs an agent from frood.yaml: a time is kept, and the console says why it does not run.
  await page.getByRole("button", { name: "Settings: Nightly pass" }).click();
  const form = page.getByRole("dialog");
  await form.getByRole("button", { name: "Add a time" }).click();
  await form.getByLabel("Time", { exact: true }).fill("02:15");
  await form.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText(/Saved, but it does not run/)).toBeVisible();
  await expect(page.getByText("changed here")).toBeVisible();

  await page.getByRole("button", { name: "Settings: Nightly pass" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Back to the default" }).click();
  await expect(page.getByText("changed here")).toHaveCount(0);
});

test("notifications from the console: how urgent, the push fuse, reminders, and my quiet hours", async ({ page }) => {
  await signIn(page, "/admin/automations");
  await page.getByRole("tab", { name: "Notifications" }).click();
  await page.getByRole("combobox", { name: "How urgent: New" }).click();
  await page.getByRole("option", { name: "Now", exact: true }).click();
  await page.getByLabel("Pushes a day, at most").fill("12");
  await page.getByRole("button", { name: "Add a reminder" }).click();
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("Notification settings saved")).toBeVisible();
  await page.reload();
  await page.getByRole("tab", { name: "Notifications" }).click();
  await expect(page.getByRole("combobox", { name: "How urgent: New" })).toHaveText(/Now/);
  await expect(page.getByLabel("Pushes a day, at most")).toHaveValue("12");

  // A person's own settings, from the account menu: the shared ones show as the default.
  await page.getByRole("button", { name: "demo@example.com" }).click();
  await page.getByRole("menuitem", { name: "My settings" }).click();
  const sheet = page.getByRole("dialog");
  await expect(sheet.getByPlaceholder("As for everyone: 12")).toBeVisible();
  await expect(sheet.getByText("As for everyone: 1 day before, at 09:00")).toBeVisible();
  await expect(sheet.getByText(/No channel is connected/)).toBeVisible();
  await sheet.getByRole("switch", { name: "Quiet hours" }).click();
  await expect(sheet.getByText("Set your time zone: quiet hours and reminders need it.")).toBeVisible();
  await sheet.getByLabel("Time zone").fill("Europe/Belgrade");
  await sheet.getByLabel("From").fill("22:30");
  await sheet.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("Your settings are saved")).toBeVisible();
  await expect(sheet.getByLabel("From")).toHaveValue("22:30");
});
