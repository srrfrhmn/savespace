import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { type FormEvent, useState } from "react";
import { Form } from "react-aria-components";
import {
  meQuery,
  setUpOwner,
  setupStatusQuery,
  signInWithPasskey,
  signInWithRecoveryCode,
} from "../auth/session";
import { passkeysSupported } from "../auth/webauthn";
import { Button, describeError, Notice, Page, TextField } from "../ui";

/** Runs an async action with a busy state and a calm error message. */
function useAction() {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  async function run(action: () => Promise<void>) {
    setBusy(true);
    setError(null);
    try {
      await action();
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }
  return { busy, error, run };
}

/** After sign-in: load the person, then go home. */
function useSignedIn() {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  return async () => {
    // Load the new session before the route check reads the cache.
    await queryClient.fetchQuery({ ...meQuery, staleTime: 0 });
    await navigate({ to: "/" });
  };
}

export function SignIn() {
  const setup = useQuery(setupStatusQuery);
  const { busy, error, run } = useAction();
  const signedIn = useSignedIn();

  return (
    <Page title="SaveSpace">
      <p className="lead">Sign in with your passkey.</p>
      {passkeysSupported() ? (
        <Button
          isPending={busy}
          onPress={() =>
            run(async () => {
              await signInWithPasskey();
              await signedIn();
            })
          }
        >
          Sign in with a passkey
        </Button>
      ) : (
        <Notice tone="problem">
          This browser does not support passkeys. Use a recent browser.
        </Notice>
      )}
      <Notice tone="problem">{error}</Notice>
      <nav className="links" aria-label="Other ways to sign in">
        <Link to="/recover">Use a recovery code</Link>
        {setup.data?.setup_open ? <Link to="/setup">Set up SaveSpace</Link> : null}
      </nav>
    </Page>
  );
}

export function Recover() {
  const [code, setCode] = useState("");
  const { busy, error, run } = useAction();
  const signedIn = useSignedIn();

  function submit(e: FormEvent) {
    e.preventDefault();
    run(async () => {
      await signInWithRecoveryCode(code);
      await signedIn();
    });
  }

  return (
    <Page title="Use a recovery code">
      <p className="lead">
        Each recovery code works one time. After you sign in, add a new passkey in Security.
      </p>
      <Form onSubmit={submit} className="form">
        <TextField
          label="Recovery code"
          description="For example 7KQ2-M9XD-4TPA. Case and dashes do not matter."
          value={code}
          onChange={setCode}
          isRequired
          autoComplete="one-time-code"
        />
        <Button type="submit" isPending={busy}>
          Sign in
        </Button>
      </Form>
      <Notice tone="problem">{error}</Notice>
      <nav className="links">
        <Link to="/sign-in">Back to sign in</Link>
      </nav>
    </Page>
  );
}

export function Setup() {
  const [setupCode, setSetupCode] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [passkeyName, setPasskeyName] = useState("This device");
  const [codes, setCodes] = useState<string[] | null>(null);
  const { busy, error, run } = useAction();
  const signedIn = useSignedIn();

  if (codes) {
    return (
      <Page title="Save your recovery codes">
        <RecoveryCodes codes={codes} />
        <Button onPress={() => run(signedIn)}>I saved my codes</Button>
      </Page>
    );
  }

  function submit(e: FormEvent) {
    e.preventDefault();
    run(async () => {
      setCodes(await setUpOwner({ setupCode, displayName, passkeyName }));
    });
  }

  return (
    <Page title="Set up SaveSpace">
      <p className="lead">
        Create the owner account. Your passkey signs you in. SaveSpace never stores a password.
      </p>
      <Form onSubmit={submit} className="form">
        <TextField
          label="Setup code"
          description="The owner-setup-code value from Secret Manager."
          value={setupCode}
          onChange={setSetupCode}
          isRequired
          autoComplete="off"
        />
        <TextField
          label="Your name"
          value={displayName}
          onChange={setDisplayName}
          isRequired
          maxLength={100}
          autoComplete="name"
        />
        <TextField
          label="Passkey name"
          description="A name that helps you know this passkey later."
          value={passkeyName}
          onChange={setPasskeyName}
          isRequired
          maxLength={100}
        />
        <Button type="submit" isPending={busy}>
          Create passkey
        </Button>
      </Form>
      <Notice tone="problem">{error}</Notice>
    </Page>
  );
}

/** Shows recovery codes once, with a copy button. */
export function RecoveryCodes({ codes }: { codes: string[] }) {
  const [copied, setCopied] = useState(false);
  return (
    <section aria-labelledby="codes-heading" className="card">
      <h2 id="codes-heading">Recovery codes</h2>
      <p>
        Save these codes in a safe place, for example a password manager. Each code works one time.
        You will not see them again.
      </p>
      <ol className="codes">
        {codes.map((code) => (
          <li key={code}>
            <code>{code}</code>
          </li>
        ))}
      </ol>
      <Button
        variant="quiet"
        onPress={async () => {
          await navigator.clipboard.writeText(codes.join("\n"));
          setCopied(true);
        }}
      >
        Copy codes
      </Button>
      <Notice>{copied ? "Codes copied." : null}</Notice>
    </section>
  );
}
