/**
 * Plaid Link (web). The script loads on demand from Plaid's CDN; the CSP
 * allows exactly this URL. SaveSpace never sees bank credentials: the person
 * signs in to the bank inside Plaid's frame (design doc §10.1).
 */

const SCRIPT_URL = "https://cdn.plaid.com/link/v2/stable/link-initialize.js";

type LinkMetadata = {
  institution?: { institution_id?: string; name?: string } | null;
};

type LinkHandler = { open: () => void; destroy: () => void };

type PlaidGlobal = {
  create: (config: {
    token: string;
    onSuccess: (publicToken: string, metadata: LinkMetadata) => void;
    onExit: (error: unknown, metadata: unknown) => void;
  }) => LinkHandler;
};

declare global {
  interface Window {
    Plaid?: PlaidGlobal;
  }
}

let loading: Promise<PlaidGlobal> | null = null;

function loadPlaid(): Promise<PlaidGlobal> {
  if (window.Plaid) {
    return Promise.resolve(window.Plaid);
  }
  loading ??= new Promise((resolve, reject) => {
    const script = document.createElement("script");
    script.src = SCRIPT_URL;
    script.async = true;
    script.onload = () => (window.Plaid ? resolve(window.Plaid) : reject(new Error("Plaid")));
    script.onerror = () => {
      loading = null;
      reject(new Error("Plaid Link did not load."));
    };
    document.head.appendChild(script);
  });
  return loading;
}

/** The result of Link: a public token and the institution, or null if closed. */
export type LinkResult = {
  publicToken: string;
  institution: { id: string; name: string } | null;
} | null;

/** Opens Plaid Link. Resolves when the person finishes or closes it. */
export async function openPlaidLink(linkToken: string): Promise<LinkResult> {
  const plaid = await loadPlaid();
  return new Promise((resolve) => {
    const handler = plaid.create({
      token: linkToken,
      onSuccess: (publicToken, metadata) => {
        const i = metadata.institution;
        resolve({
          publicToken,
          institution: i?.institution_id && i.name ? { id: i.institution_id, name: i.name } : null,
        });
        handler.destroy();
      },
      onExit: () => {
        resolve(null);
        handler.destroy();
      },
    });
    handler.open();
  });
}
