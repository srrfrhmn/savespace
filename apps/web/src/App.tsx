import { coreVersion, formatMoney } from "@savespace/savespace-core-wasm";

/**
 * Phase 0 shell. It proves the web app renders money through savespace-core.
 * Phase 1 replaces it with routes (TanStack Router) and real data.
 */
export function App() {
  const thisWeek = formatMoney(21_200n, "USD");
  const usual = formatMoney(26_800n, "USD");

  return (
    <main className="shell">
      <h1>SaveSpace</h1>
      <section className="insight" aria-label="Example insight">
        <p>
          You spent less than usual this week: {thisWeek} so far, compared to a usual {usual}.
        </p>
      </section>
      <p className="meta">Example data. savespace-core {coreVersion()} (WASM).</p>
    </main>
  );
}
