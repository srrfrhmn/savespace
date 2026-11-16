import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { type FormEvent, useState } from "react";
import { Form } from "react-aria-components";
import { addPasskey, deletePasskey, meQuery, newRecoveryCodes, signOut } from "../auth/session";
import { CalmCard } from "../budget/calm";
import { budgetQuery, monthKey, recurringQuery, setCalmMode } from "../budget/data";
import { accountsQuery, type Connection, connectionsQuery } from "../connections/data";
import { bankDate } from "../format";
import { CheckInCard } from "../insights/checkin";
import { homeInsightsQuery, mutesQuery, unmute } from "../insights/data";
import { InsightFeed } from "../insights/feed";
import { Button, describeError, Notice, Page, TextField } from "../ui";
import { RecoveryCodes } from "./auth";
import { seriesLine, upcoming } from "./recurring";

const dateFormat = new Intl.DateTimeFormat("en-US", { dateStyle: "medium" });

function formatDate(value: string | null | undefined): string {
  return value ? dateFormat.format(new Date(value)) : "Never";
}

function useSignOut() {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  return async () => {
    await signOut();
    queryClient.setQueryData(meQuery.queryKey, null);
    await navigate({ to: "/sign-in" });
  };
}

export function Home() {
  const queryClient = useQueryClient();
  const me = useQuery(meQuery).data;
  const doSignOut = useSignOut();

  const connections = useQuery(connectionsQuery).data;
  const accounts = useQuery(accountsQuery).data;
  const ready = (connections?.length ?? 0) > 0;
  const budget = useQuery({ ...budgetQuery(monthKey(new Date())), enabled: ready });
  const recurring = useQuery({ ...recurringQuery, enabled: ready });
  const soon = upcoming(recurring.data ?? [], 7).slice(0, 3);
  const [calmError, setCalmError] = useState<string | null>(null);

  /** Optimistic: the switch moves at once and moves back if the save fails. */
  async function toggleCalm(on: boolean) {
    setCalmError(null);
    const before = queryClient.getQueryData(meQuery.queryKey);
    queryClient.setQueryData(meQuery.queryKey, (old) => (old ? { ...old, calm_mode: on } : old));
    try {
      queryClient.setQueryData(meQuery.queryKey, await setCalmMode(on));
    } catch (e) {
      queryClient.setQueryData(meQuery.queryKey, before);
      setCalmError(describeError(e));
    }
  }

  const insights = useQuery({ ...homeInsightsQuery, enabled: ready });
  const calm = budget.data?.calm;
  return (
    <Page title={me ? `Hello, ${me.display_name}` : "SaveSpace"}>
      {calm && budget.data ? (
        <CalmCard calm={calm} currency={budget.data.currency} calmMode={me?.calm_mode ?? false} />
      ) : (
        <section className="card" aria-label="Status">
          <HomeStatus connections={connections} accountCount={accounts?.length ?? 0} />
        </section>
      )}
      <InsightFeed insights={insights.data ?? []} />
      {ready ? <CheckInCard /> : null}
      {soon.length > 0 ? (
        <section className="card" aria-labelledby="soon-h">
          <h2 id="soon-h">Coming up this week</h2>
          <ul className="list">
            {soon.map((s) => (
              <li key={s.id}>
                <span>
                  <strong>{s.merchant_name}</strong>
                  <span className="meta">
                    {bankDate(s.next_date)}. {seriesLine(s)}
                  </span>
                </span>
              </li>
            ))}
          </ul>
        </section>
      ) : null}
      {me ? (
        <label className="check">
          <input
            type="checkbox"
            checked={me.calm_mode}
            onChange={(e) => void toggleCalm(e.target.checked)}
          />
          Calm mode: show a word instead of balances
        </label>
      ) : null}
      <Notice tone="problem">{calmError}</Notice>
      <nav className="links" aria-label="Main">
        <Link to="/budget">Budget</Link>
        <Link to="/transactions">Transactions</Link>
        <Link to="/recurring">Recurring</Link>
        <Link to="/reflection">Weekly reflection</Link>
        <Link to="/accounts">Accounts</Link>
        <Link to="/settings/security">Security</Link>
        <Button variant="quiet" onPress={doSignOut}>
          Sign out
        </Button>
      </nav>
    </Page>
  );
}

/** One calm status line. Insights replace it in M5. */
function HomeStatus({
  connections,
  accountCount,
}: {
  connections: Connection[] | undefined;
  accountCount: number;
}) {
  if (!connections) {
    return <p>Loading.</p>;
  }
  if (connections.length === 0) {
    return (
      <p>
        Connect a bank account to start. <Link to="/accounts">Connect an account</Link>
      </p>
    );
  }
  if (connections.some((c) => c.status === "syncing")) {
    return <p>Getting your accounts and transactions. This can take a minute.</p>;
  }
  const attention = connections.filter((c) =>
    ["login_required", "pending_expiration", "revoked"].includes(c.status),
  );
  if (attention.length > 0) {
    return (
      <p>
        A bank connection needs you to sign in again. <Link to="/accounts">Open accounts</Link>
      </p>
    );
  }
  return (
    <p>
      {accountCount === 1 ? "1 account is" : `${accountCount} accounts are`} connected and up to
      date. <Link to="/budget">Open your budget</Link>
    </p>
  );
}

/** Plain names for insight rules, for the muted list. */
const RULE_NAMES: Record<string, string> = {
  "spend.week_below_usual": "Weeks with less spending than usual",
  "spend.week_above_usual": "Weeks with more spending than usual",
  "budget.month_on_track": "On track for the monthly budget",
  "txn.recurring_expected": "Expected recurring charges",
  "txn.recurring_higher": "Recurring charges higher than usual",
  "recurring.upcoming": "Upcoming recurring charges",
  "balance.may_not_cover": "Cash that may not cover a bill",
};

export function Security() {
  const queryClient = useQueryClient();
  const me = useQuery(meQuery).data;
  const mutes = useQuery(mutesQuery);
  const [name, setName] = useState("");
  const [codes, setCodes] = useState<string[] | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function act(id: string, action: () => Promise<void>, done: string) {
    setBusy(id);
    setError(null);
    setMessage(null);
    try {
      await action();
      await queryClient.invalidateQueries({ queryKey: meQuery.queryKey });
      setMessage(done);
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(null);
    }
  }

  function submitAdd(e: FormEvent) {
    e.preventDefault();
    act(
      "add",
      async () => {
        await addPasskey(name);
        setName("");
      },
      "Passkey added.",
    );
  }

  if (!me) {
    return null;
  }

  return (
    <Page title="Security">
      <section className="card" aria-labelledby="passkeys-heading">
        <h2 id="passkeys-heading">Passkeys</h2>
        <ul className="list">
          {me.passkeys.map((p) => (
            <li key={p.id}>
              <span>
                <strong>{p.name}</strong>
                <span className="meta">
                  Added {formatDate(p.created_at)}. Last used {formatDate(p.last_used_at)}.
                </span>
              </span>
              <Button
                variant="quiet"
                isDisabled={me.passkeys.length < 2}
                isPending={busy === p.id}
                onPress={() => act(p.id, () => deletePasskey(p.id), "Passkey removed.")}
                aria-label={`Remove passkey ${p.name}`}
              >
                Remove
              </Button>
            </li>
          ))}
        </ul>
        <Form onSubmit={submitAdd} className="form">
          <TextField
            label="New passkey name"
            value={name}
            onChange={setName}
            isRequired
            maxLength={100}
          />
          <Button type="submit" isPending={busy === "add"}>
            Add a passkey
          </Button>
        </Form>
      </section>

      <section className="card" aria-labelledby="recovery-heading">
        <h2 id="recovery-heading">Recovery codes</h2>
        <p>You have {me.recovery_codes_left} unused recovery codes.</p>
        <Button
          variant="quiet"
          isPending={busy === "codes"}
          onPress={() =>
            act(
              "codes",
              async () => {
                setCodes(await newRecoveryCodes());
              },
              "New recovery codes are ready. The old codes no longer work.",
            )
          }
        >
          Make new recovery codes
        </Button>
        {codes ? <RecoveryCodes codes={codes} /> : null}
      </section>

      {mutes.data && mutes.data.length > 0 ? (
        <section className="card" aria-labelledby="mutes-heading">
          <h2 id="mutes-heading">Muted insights</h2>
          <p>You marked these kinds of insights as not helpful.</p>
          <ul className="list">
            {mutes.data.map((rule) => (
              <li key={rule}>
                <span>{RULE_NAMES[rule] ?? rule}</span>
                <Button
                  variant="quiet"
                  isPending={busy === rule}
                  onPress={() =>
                    act(
                      rule,
                      async () => {
                        await unmute(rule);
                        await queryClient.invalidateQueries({ queryKey: ["insights"] });
                      },
                      "You will see this kind of insight again.",
                    )
                  }
                >
                  Show again
                </Button>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      <Notice>{message}</Notice>
      <Notice tone="problem">{error}</Notice>
      <nav className="links">
        <Link to="/">Back to home</Link>
      </nav>
    </Page>
  );
}
