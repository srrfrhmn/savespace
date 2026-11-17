import AxeBuilder from "@axe-core/playwright";
import { type BrowserContext, expect, type Page } from "@playwright/test";

/** WCAG 2.2 AA (design doc §8.4). */
const WCAG = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];

/** Fails on any axe violation, with a short list of what failed. */
export async function expectAccessible(page: Page, name: string): Promise<void> {
  // Light and dark use different color tokens, so check both.
  for (const colorScheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme });
    // React Aria's hidden live announcer keeps a node that points at a
    // button label after the button is gone. It was read out already.
    const result = await new AxeBuilder({ page })
      .withTags(WCAG)
      .exclude("[data-live-announcer]")
      .analyze();
    const problems = result.violations.map(
      (v) =>
        `${v.id} (${v.impact}): ${v.help} — ${v.nodes.map((n) => n.html.slice(0, 160)).join(" | ")}`,
    );
    expect(problems, `axe on ${name} (${colorScheme})`).toEqual([]);
  }
  await page.emulateMedia({ colorScheme: null });
}

/** Fails if the page scrolls sideways at phone width (375 px). */
export async function expectNoSidewaysScroll(page: Page, name: string): Promise<void> {
  const size = page.viewportSize();
  await page.setViewportSize({ width: 375, height: 800 });
  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth - window.innerWidth,
  );
  expect(overflow, `sideways scroll on ${name} at 375 px`).toBeLessThanOrEqual(0);
  if (size) {
    await page.setViewportSize(size);
  }
}

/** Adds a virtual passkey authenticator (Chromium only). */
export async function addVirtualAuthenticator(context: BrowserContext, page: Page): Promise<void> {
  const cdp = await context.newCDPSession(page);
  await cdp.send("WebAuthn.enable");
  await cdp.send("WebAuthn.addVirtualAuthenticator", {
    options: {
      protocol: "ctap2",
      transport: "internal",
      hasResidentKey: true,
      hasUserVerification: true,
      isUserVerified: true,
      automaticPresenceSimulation: true,
    },
  });
}

/** Collects console errors, page errors, and CSP violations. */
export function watchProblems(page: Page): string[] {
  const problems: string[] = [];
  page.on("console", (m) => {
    if (m.type() !== "error") {
      return;
    }
    const url = m.location().url;
    // Expected: the signed-out /v1/me check (401), the wrong setup code
    // (403), the step-up prompt before an export (403), and the refused
    // duplicate bank (409). Plaid's Link script adds an inline <style> that
    // the CSP blocks (its hash changes); Link works without it, and the CSP
    // stays strict.
    const expected =
      (m.text().includes("401") && url.endsWith("/v1/me")) ||
      (m.text().includes("403") && url.endsWith("/v1/auth/setup/start")) ||
      (m.text().startsWith("Applying inline style violates") &&
        url.startsWith("https://cdn.plaid.com/link/")) ||
      (m.text().startsWith("CSP style-src-elem inline https://cdn.plaid.com/link/") &&
        page.url().endsWith("/accounts")) ||
      (m.text().includes("403") && url.endsWith("/v1/exports")) ||
      (m.text().includes("409") && url.endsWith("/v1/connections"));
    if (!expected) {
      problems.push(`${m.text()} (${url || page.url()})`);
    }
  });
  page.on("pageerror", (e) => problems.push(`pageerror: ${e.message}`));
  void page.addInitScript(() => {
    document.addEventListener("securitypolicyviolation", (e) =>
      console.error(
        `CSP ${e.violatedDirective} ${e.blockedURI} ${e.sourceFile}:${e.lineNumber} ${e.sample}`,
      ),
    );
  });
  return problems;
}

const PLAID_HOSTS: Record<string, string> = {
  sandbox: "https://sandbox.plaid.com",
};

/**
 * A Plaid sandbox public token for First Platypus Bank (`user_good`), made
 * without the Link UI, so the test does not depend on Link's screens.
 */
export async function sandboxPublicToken(): Promise<string> {
  const env = process.env.PLAID_ENV ?? "sandbox";
  const host = PLAID_HOSTS[env];
  const clientId = process.env.PLAID_CLIENT_ID;
  const secret = process.env.PLAID_SECRET;
  if (!host || !clientId || !secret) {
    throw new Error("Set PLAID_CLIENT_ID and PLAID_SECRET for the Plaid sandbox.");
  }
  const response = await fetch(`${host}/sandbox/public_token/create`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      client_id: clientId,
      secret,
      institution_id: "ins_109508",
      initial_products: ["transactions"],
    }),
  });
  const body = (await response.json()) as { public_token?: string; error_message?: string };
  if (!response.ok || !body.public_token) {
    throw new Error(`Plaid sandbox: ${body.error_message ?? response.status}`);
  }
  return body.public_token;
}
