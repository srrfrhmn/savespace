import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import {
  type Account,
  accountsQuery,
  type Connection,
  connectBank,
  connectionsQuery,
  recentTransactionsQuery,
  refreshConnection,
  removeConnection,
  repairConnection,
} from "../connections/data";
import { bankDate, money } from "../format";
import { Button, describeError, Notice, Page } from "../ui";

const relative = new Intl.RelativeTimeFormat("en-US", { numeric: "auto" });

function ago(value: string | null | undefined): string {
  if (!value) {
    return "not yet";
  }
  const minutes = Math.round((Date.parse(value) - Date.now()) / 60_000);
  if (Math.abs(minutes) < 60) {
    return relative.format(minutes, "minute");
  }
  const hours = Math.round(minutes / 60);
  if (Math.abs(hours) < 48) {
    return relative.format(hours, "hour");
  }
  return relative.format(Math.round(hours / 24), "day");
}

/** One calm sentence for the connection state. Facts first (Appendix B). */
function statusText(c: Connection): { text: string; needsAction: boolean } {
  switch (c.status) {
    case "syncing":
      return {
        text: "Getting your accounts and transactions. This can take a minute.",
        needsAction: false,
      };
    case "ok":
      return { text: `Up to date. Updated ${ago(c.last_synced_at)}.`, needsAction: false };
    case "login_required":
      return { text: "Sign in to your bank again to keep this up to date.", needsAction: true };
    case "pending_expiration":
      return {
        text: "Your bank asks you to sign in again soon to keep this connected.",
        needsAction: true,
      };
    case "revoked":
      return {
        text: "Access was turned off at the bank. Remove this connection, then connect again.",
        needsAction: true,
      };
    default:
      return {
        text: `The last update did not work. SaveSpace tries again. Last update ${ago(c.last_synced_at)}.`,
        needsAction: false,
      };
  }
}

function accountLine(a: Account): string {
  const kind = a.subtype ?? a.type;
  return a.mask ? `${kind} ending in ${a.mask}` : kind;
}

export function Accounts() {
  const queryClient = useQueryClient();
  const connections = useQuery(connectionsQuery);
  const accounts = useQuery(accountsQuery);
  const recent = useQuery(recentTransactionsQuery);
  const [busy, setBusy] = useState<string | null>(null);
  const [confirming, setConfirming] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const syncing = connections.data?.some((c) => c.status === "syncing") ?? false;

  async function act(id: string, action: () => Promise<unknown>, done: string | null) {
    setBusy(id);
    setError(null);
    setMessage(null);
    try {
      const result = await action();
      await queryClient.invalidateQueries();
      if (result !== false && done) {
        setMessage(done);
      }
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(null);
      setConfirming(null);
    }
  }

  // When a sync finishes, the lists update.
  const lastSynced = connections.data?.map((c) => `${c.id}:${c.last_synced_at}`).join();
  useEffect(() => {
    if (lastSynced !== undefined) {
      void queryClient.invalidateQueries({ queryKey: ["accounts"] });
      void queryClient.invalidateQueries({ queryKey: ["transactions"] });
    }
  }, [lastSynced, queryClient]);

  return (
    <Page title="Accounts">
      <p className="lead">
        Connect your bank and card accounts. SaveSpace reads balances and transactions. It cannot
        move money.
      </p>
      <Button
        isPending={busy === "connect"}
        onPress={() => act("connect", connectBank, "Connected. Getting your transactions now.")}
      >
        Connect an account
      </Button>
      <Notice>{message}</Notice>
      <Notice tone="problem">{error}</Notice>

      {connections.data?.length === 0 ? (
        <section className="card">
          <p>No accounts yet. Connect one to start.</p>
        </section>
      ) : null}

      {connections.data?.map((c) => {
        const status = statusText(c);
        const own = accounts.data?.filter((a) => a.connection_id === c.id) ?? [];
        return (
          <section key={c.id} className="card" aria-labelledby={`${c.id}-name`}>
            <h2 id={`${c.id}-name`}>{c.institution_name ?? "Bank connection"}</h2>
            <p className={status.needsAction ? "status status-action" : "status"}>{status.text}</p>
            {own.length > 0 ? (
              <ul className="list">
                {own.map((a) => (
                  <li key={a.id}>
                    <span>
                      <strong>{a.name}</strong>
                      <span className="meta">{accountLine(a)}</span>
                    </span>
                    <span className="amount">{money(a.current_minor, a.currency)}</span>
                  </li>
                ))}
              </ul>
            ) : null}
            <div className="actions">
              {status.needsAction && c.status !== "revoked" ? (
                <Button
                  isPending={busy === `repair-${c.id}`}
                  onPress={() =>
                    act(`repair-${c.id}`, () => repairConnection(c.id), "Thanks. Updating now.")
                  }
                >
                  Sign in to your bank
                </Button>
              ) : null}
              {c.status !== "syncing" && c.status !== "revoked" ? (
                <Button
                  variant="quiet"
                  isPending={busy === `sync-${c.id}`}
                  onPress={() =>
                    act(`sync-${c.id}`, () => refreshConnection(c.id), "Updating now.")
                  }
                >
                  Update now
                </Button>
              ) : null}
              {confirming === c.id ? (
                <fieldset className="confirm">
                  <legend>Remove this connection and its accounts and transactions?</legend>
                  <Button
                    isPending={busy === `remove-${c.id}`}
                    onPress={() =>
                      act(`remove-${c.id}`, () => removeConnection(c.id), "Connection removed.")
                    }
                  >
                    Remove
                  </Button>
                  <Button variant="quiet" onPress={() => setConfirming(null)}>
                    Keep it
                  </Button>
                </fieldset>
              ) : (
                <Button variant="quiet" onPress={() => setConfirming(c.id)}>
                  Remove
                </Button>
              )}
            </div>
          </section>
        );
      })}

      {recent.data && recent.data.data.length > 0 && !syncing ? (
        <section className="card" aria-labelledby="recent-heading">
          <h2 id="recent-heading">Recent transactions</h2>
          <ul className="list">
            {recent.data.data.map((t) => (
              <li key={t.id}>
                <span>
                  <strong>{t.merchant_name ?? t.name}</strong>
                  <span className="meta">
                    {bankDate(t.date)}
                    {t.pending ? ". Pending" : ""}
                  </span>
                </span>
                <span className="amount">{money(t.amount_minor, t.currency)}</span>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      <nav className="links">
        <Link to="/transactions">All transactions</Link>
        <Link to="/">Back to home</Link>
      </nav>
    </Page>
  );
}
