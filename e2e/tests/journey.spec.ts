import { type BrowserContext, expect, type Page, test } from "@playwright/test";
import {
  addVirtualAuthenticator,
  expectAccessible,
  expectNoSidewaysScroll,
  sandboxPublicToken,
  watchProblems,
} from "./helpers";

/**
 * The Phase 1 journey against the Plaid sandbox (design doc §14.3): setup,
 * connect, sync, transactions, budget, recurring, insights, reflection,
 * review, settings, export, and account deletion. Every screen gets an axe
 * check (WCAG 2.2 AA) and a phone-width check.
 *
 * Needs an empty database: it sets up the owner and deletes the account at
 * the end, so it can run again.
 */
test.describe.configure({ mode: "serial" });

const SETUP_CODE = process.env.E2E_SETUP_CODE ?? "local-setup-code";

let context: BrowserContext;
let page: Page;
let problems: string[];

test.beforeAll(async ({ browser }) => {
  context = await browser.newContext({ acceptDownloads: true, timezoneId: "America/New_York" });
  page = await context.newPage();
  problems = watchProblems(page);
  await addVirtualAuthenticator(context, page);
});

test.afterAll(async () => {
  await context.close();
});

async function checkScreen(name: string) {
  await expectAccessible(page, name);
  await expectNoSidewaysScroll(page, name);
}

test("signed out: sign-in, setup, help, and privacy", async () => {
  await page.goto("/");
  await page.waitForURL("**/sign-in");
  await expect(page.getByRole("button", { name: "Sign in with a passkey" })).toBeVisible();
  await checkScreen("sign-in");
  await page.goto("/recover");
  await expect(page.getByLabel("Recovery code")).toBeVisible();
  await checkScreen("recover");
  await page.goto("/help");
  await expect(page.getByRole("heading", { name: "Help and support" })).toBeVisible();
  await expect(page.getByRole("link", { name: "988.ca" })).toBeVisible();
  await expect(page.getByRole("link", { name: "Credit Counselling Canada" })).toBeVisible();
  await checkScreen("help");
  await page.goto("/privacy");
  await expect(page.getByRole("heading", { name: "Privacy notice" })).toBeVisible();
  await checkScreen("privacy");
  await page.goto("/setup");
  await expect(page.getByLabel("Setup code")).toBeVisible();
  await checkScreen("setup");
});

test("setup with a passkey", async () => {
  await page.goto("/setup");
  await page.getByLabel("Setup code").fill("not-the-code");
  await page.getByLabel("Your name").fill("Sam");
  await page.getByRole("button", { name: "Create passkey" }).click();
  await expect(page.getByText("That setup code is not correct")).toBeVisible();

  await page.getByLabel("Setup code").fill(SETUP_CODE);
  await page.getByRole("button", { name: "Create passkey" }).click();
  await expect(page.getByRole("heading", { name: "Save your recovery codes" })).toBeVisible();
  await expect(page.locator(".codes code")).toHaveCount(10);
  await checkScreen("recovery codes");
  await page.getByRole("button", { name: "I saved my codes" }).click();
  await expect(page.getByRole("heading", { name: "Hello, Sam" })).toBeVisible();
  await checkScreen("home (empty)");
});

test("connect a sandbox bank and sync", async () => {
  await page.goto("/accounts");
  await expect(page.getByRole("heading", { name: "Accounts" })).toBeVisible();
  await checkScreen("accounts (empty)");

  // Plaid Link opens (link token, CSP), then the test leaves it.
  await page.getByRole("button", { name: "Connect an account" }).click();
  await expect(page.locator('iframe[id^="plaid-link-iframe"]').first()).toBeVisible({
    timeout: 30_000,
  });
  await page.goto("/accounts");

  // Connect with a sandbox public token, as Link's onSuccess would.
  const publicToken = await sandboxPublicToken();
  const status = await page.evaluate(async (token) => {
    const response = await fetch("/v1/connections", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        public_token: token,
        institution: { id: "ins_109508", name: "First Platypus Bank" },
      }),
    });
    return response.status;
  }, publicToken);
  expect(status).toBe(201);
  await page.reload();
  await expect(page.getByText(/Up to date\. Updated/)).toBeVisible({ timeout: 120_000 });

  // The sandbox may need a moment before transactions are ready.
  const recent = page.getByRole("heading", { name: "Recent transactions" });
  for (let i = 0; i < 6 && !(await recent.isVisible()); i++) {
    await page.getByRole("button", { name: "Update now" }).click();
    await page.waitForTimeout(10_000);
    await page.reload();
  }
  await expect(recent).toBeVisible();
  await checkScreen("accounts");

  // The same bank again is refused, so nothing counts twice.
  const again = await sandboxPublicToken();
  const duplicate = await page.evaluate(async (token) => {
    const response = await fetch("/v1/connections", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        public_token: token,
        institution: { id: "ins_109508", name: "First Platypus Bank" },
      }),
    });
    return { status: response.status, body: await response.json() };
  }, again);
  expect(duplicate.status).toBe(409);
  expect(duplicate.body.error.code).toBe("already_connected");
});

test("transactions: edit a category", async () => {
  await page.goto("/transactions");
  const rows = page.locator("ul.txns > li");
  await expect(rows.first()).toBeVisible();
  await checkScreen("transactions");

  await rows.first().locator("button.txn-row").click();
  const panel = page.getByRole("region", { name: "Edit transaction" });
  await expect(panel).toBeVisible();
  await checkScreen("transaction edit panel");
  await panel.getByLabel("Category").selectOption({ label: "Travel" });
  await panel.getByRole("button", { name: "Save" }).click();
  const rule = page.getByRole("region", { name: "Category rule" });
  await expect(rule).toBeVisible();
  await rule.getByRole("button", { name: "Just this one" }).click();
  await expect(rows.first()).toContainText("Travel");

  await page.goto("/rules");
  await expect(page.getByRole("heading", { name: "Category rules" })).toBeVisible();
  await checkScreen("rules");
});

test("budget, recurring, and home", async () => {
  await page.goto("/budget");
  await expect(page.getByRole("heading", { name: "Flexible" })).toBeVisible();
  const dining = page.locator(".budget-row").filter({ hasText: "Restaurants and coffee" });
  await dining.getByRole("button", { name: "Plan" }).click();
  const editor = page.getByRole("form", { name: "Plan for Restaurants and coffee" });
  await editor.getByLabel("Monthly plan").fill("150");
  await editor.getByRole("button", { name: "Save plan" }).click();
  await expect(page.getByText(/^Saved\. The plan for Restaurants and coffee/)).toBeVisible();
  await checkScreen("budget");

  await page.goto("/recurring");
  await expect(page.getByRole("heading", { name: "Recurring", exact: true })).toBeVisible();
  await checkScreen("recurring");

  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Hello, Sam" })).toBeVisible();
  await expect(page.getByRole("region", { name: "Money status" })).toBeVisible();
  await checkScreen("home");
  // Click, then wait: a /me refetch can land between the click and the save.
  const calm = page.getByLabel("Calm mode: show a word instead of balances");
  await calm.click();
  await expect(calm).toBeChecked();
  await checkScreen("home (calm mode)");
  await calm.click();
  await expect(calm).not.toBeChecked();
});

test("check-in, reflection, and the insight review", async () => {
  await page.goto("/");
  const checkIn = page.getByRole("group", { name: "How do you feel about money today?" });
  await checkIn.getByLabel("4: At ease").check();
  await page.getByRole("button", { name: "Save check-in" }).click();
  await expect(page.getByText("Saved. Thank you for checking in.")).toBeVisible();

  await page.goto("/reflection");
  await expect(page.getByRole("heading", { name: "Weekly reflection" })).toBeVisible();
  await expect(page.locator("ol.reflection li").first()).toBeVisible();
  await checkScreen("reflection");

  // Insights come from the worker after the sync (outbox chain).
  await page.goto("/review");
  await expect(page.getByRole("heading", { name: "Insight review" })).toBeVisible();
  const cards = page.locator(".review-card");
  for (let i = 0; i < 12 && (await cards.count()) === 0; i++) {
    await page.waitForTimeout(5_000);
    await page.reload();
  }
  await expect(cards.first()).toBeVisible();
  await cards.first().getByLabel("Correct").check();
  await expect(page.getByText(/^100\.00% correct: 1 correct, 0 wrong\./)).toBeVisible();
  await page.reload();
  await expect(page.getByText(/^100\.00% correct: 1 correct, 0 wrong\./)).toBeVisible();
  await checkScreen("review");
});

test("settings and export", async () => {
  await page.goto("/settings");
  await expect(page.getByRole("heading", { name: "Settings" })).toBeVisible();
  await checkScreen("settings");
  await page.getByLabel("Time zone").selectOption("America/Chicago");
  await expect(page.getByText("Saved.")).toBeVisible();

  // The household currency: accounts in another one get a note.
  await page.getByLabel("Currency").selectOption("CAD");
  await expect(page.getByLabel("Currency")).toHaveValue("CAD");
  await page.goto("/accounts");
  await expect(
    page.getByText(/Some accounts are in USD, but budgets and insights count CAD/),
  ).toBeVisible();
  await checkScreen("accounts (currency note)");
  await page.goto("/settings");
  await page.getByLabel("Currency").selectOption("USD");
  await expect(page.getByLabel("Currency")).toHaveValue("USD");
  await page.goto("/accounts");
  await expect(page.getByRole("heading", { name: "Recent transactions" })).toBeVisible();
  await expect(page.getByText(/Some accounts are in/)).toHaveCount(0);
  await page.goto("/settings");

  const download = page.waitForEvent("download");
  await page.getByRole("button", { name: "Download my data" }).click();
  expect((await download).suggestedFilename()).toMatch(/^savespace-export-.*\.zip$/);

  await page.goto("/settings/security");
  await expect(page.getByRole("heading", { name: "Passkeys" })).toBeVisible();
  await checkScreen("security");
});

test("delete the account", async () => {
  await page.goto("/settings");
  await page.getByRole("button", { name: "Delete my account" }).click();
  await page.getByRole("button", { name: "Delete everything" }).click();
  await expect(page.getByRole("heading", { name: "Account deleted" })).toBeVisible({
    timeout: 30_000,
  });
  await checkScreen("deleted");
  await page.goto("/");
  await page.waitForURL("**/sign-in");
});

test("no console errors or CSP violations", () => {
  expect(problems).toEqual([]);
});
