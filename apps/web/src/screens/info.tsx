import { Link } from "@tanstack/react-router";
import { Page } from "../ui";

/** Help and support (design doc §5: "not a medical service", counseling, 988). */
export function Help() {
  return (
    <Page title="Help and support">
      <section className="card">
        <h2>SaveSpace is not a medical service</h2>
        <p>
          SaveSpace shows facts about your money in a calm way. It is not therapy, not medical care,
          and not financial advice.
        </p>
      </section>
      <section className="card">
        <h2>If money feels too heavy</h2>
        <p>
          If you are in crisis or thinking about suicide, call or text <strong>988</strong>. It is
          free and open all day, every day, in Canada and the US. In an emergency, call 911.
        </p>
        <h3>In Canada</h3>
        <ul className="plain">
          <li>
            Free or low-cost credit counselling from a not-for-profit agency:{" "}
            <a href="https://creditcounsellingcanada.ca/" rel="noreferrer" target="_blank">
              Credit Counselling Canada
            </a>
            .
          </li>
          <li>
            9-8-8 Suicide Crisis Helpline, in English and French:{" "}
            <a href="https://988.ca/" rel="noreferrer" target="_blank">
              988.ca
            </a>
            .
          </li>
        </ul>
        <h3>In the US</h3>
        <ul className="plain">
          <li>
            Free or low-cost financial counseling from a nonprofit:{" "}
            <a href="https://www.nfcc.org/" rel="noreferrer" target="_blank">
              National Foundation for Credit Counseling (NFCC)
            </a>
            .
          </li>
          <li>
            988 Suicide and Crisis Lifeline, also by chat:{" "}
            <a href="https://988lifeline.org/" rel="noreferrer" target="_blank">
              988lifeline.org
            </a>
            .
          </li>
        </ul>
      </section>
      <section className="card">
        <h2>Using SaveSpace</h2>
        <ul className="plain">
          <li>A bank connection needs you: open Accounts and select "Sign in to your bank".</li>
          <li>A category is wrong: open the transaction and change it. You can make a rule.</li>
          <li>An insight is wrong: select "This is wrong". SaveSpace hides it and checks it.</li>
          <li>Lost your passkey: sign in with a recovery code.</li>
        </ul>
      </section>
      <nav className="links">
        <Link to="/privacy">Privacy notice</Link>
        <Link to="/">Back to home</Link>
      </nav>
    </Page>
  );
}

/** The privacy notice (design doc §12.6). */
export function Privacy() {
  return (
    <Page title="Privacy notice">
      <section className="card">
        <h2>What SaveSpace stores</h2>
        <ul className="plain">
          <li>Your name, time zone, and settings.</li>
          <li>
            Accounts, balances, and transactions from the banks you connect through Plaid. Your bank
            password never reaches SaveSpace.
          </li>
          <li>Your edits, notes, budgets, rules, and check-ins.</li>
          <li>Passkey public keys. Never your device's private key.</li>
        </ul>
      </section>
      <section className="card">
        <h2>How it is protected</h2>
        <ul className="plain">
          <li>Bank access tokens and check-in notes are encrypted with a key for your account.</li>
          <li>Data travels over TLS. Backups are encrypted and expire after 35 days.</li>
          <li>
            Your data is stored and processed in the United States (Google Cloud and Neon, US East),
            also if you live in Canada.
          </li>
          <li>SaveSpace does not sell or share your data, and shows no ads.</li>
          <li>Insight text comes from fixed templates. Your data is not sent to an AI service.</li>
        </ul>
      </section>
      <section className="card">
        <h2>Your rights</h2>
        <ul className="plain">
          <li>See and download all your data: Settings, "Download my data".</li>
          <li>Correct it: edit transactions, categories, and settings in the app.</li>
          <li>
            Delete it: Settings, "Delete my account". This also removes the connections at Plaid.
          </li>
        </ul>
      </section>
      <section className="card">
        <h2>Service providers</h2>
        <p>
          Plaid (bank connections), Google Cloud (hosting), and Neon (database). Each one gets only
          what it needs to run its part.
        </p>
      </section>
      <nav className="links">
        <Link to="/help">Help and support</Link>
        <Link to="/">Back to home</Link>
      </nav>
    </Page>
  );
}

/** After account deletion. */
export function Deleted() {
  return (
    <Page title="Account deleted">
      <p className="lead">
        Your account and all your data are deleted, and your bank connections are removed.
      </p>
      <p>Encrypted backups expire within 35 days. Nobody can read your data in them.</p>
    </Page>
  );
}
