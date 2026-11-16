import { formatMoney, parseMoney } from "@savespace/savespace-core-wasm";

/** Money through savespace-core (WASM), the one source of formatting (§8.4). */
export function money(minor: number | null | undefined, currency: string): string {
  if (minor === null || minor === undefined) {
    return "Not available";
  }
  try {
    return formatMoney(BigInt(minor), currency);
  } catch {
    return "Not available";
  }
}

/**
 * Minor units from what a person typed ("12.50", "$1,200"), through
 * savespace-core. Returns null if it is not an amount in this currency.
 */
export function parseAmount(text: string, currency: string): number | null {
  try {
    const minor = parseMoney(text, currency);
    if (minor > BigInt(Number.MAX_SAFE_INTEGER) || minor < BigInt(Number.MIN_SAFE_INTEGER)) {
      return null;
    }
    return Number(minor);
  } catch {
    return null;
  }
}

const shortDate = new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric" });

/** "Nov 3" for a bank date (YYYY-MM-DD). Noon avoids time-zone shifts. */
export function bankDate(value: string): string {
  return shortDate.format(new Date(`${value}T12:00:00`));
}
