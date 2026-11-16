import type { ReactNode } from "react";
import {
  Button as AriaButton,
  type ButtonProps as AriaButtonProps,
  TextField as AriaTextField,
  type TextFieldProps as AriaTextFieldProps,
  Input,
  Label,
  Text,
} from "react-aria-components";
import { ApiFailure } from "./api/client";
import { PasskeyCancelled } from "./auth/webauthn";

/** Accessible components from React Aria with the SaveSpace styles. */

type ButtonProps = AriaButtonProps & { variant?: "primary" | "quiet" };

export function Button({ variant = "primary", className, ...props }: ButtonProps) {
  return <AriaButton {...props} className={`button button-${variant} ${className ?? ""}`} />;
}

type TextFieldProps = AriaTextFieldProps & {
  label: string;
  description?: string;
  inputMode?: "text" | "numeric";
};

export function TextField({ label, description, inputMode, ...props }: TextFieldProps) {
  return (
    <AriaTextField {...props} className="field">
      <Label>{label}</Label>
      <Input inputMode={inputMode} />
      {description ? <Text slot="description">{description}</Text> : null}
    </AriaTextField>
  );
}

export function Page({ title, children }: { title: string; children: ReactNode }) {
  return (
    <main className="shell">
      <h1>{title}</h1>
      {children}
    </main>
  );
}

/** A polite status message for screen readers and sighted people. */
export function Notice({
  children,
  tone = "neutral",
}: {
  children: ReactNode;
  tone?: "neutral" | "problem";
}) {
  if (!children) {
    return null;
  }
  return (
    <p className={`notice notice-${tone}`} role={tone === "problem" ? "alert" : "status"}>
      {children}
    </p>
  );
}

/** Plain-language text for an error. Calm and specific (design doc Appendix B). */
export function describeError(error: unknown): string {
  if (error instanceof PasskeyCancelled) {
    return "The passkey request stopped. You can try again.";
  }
  if (error instanceof ApiFailure) {
    switch (error.code) {
      case "setup_code_invalid":
        return "That setup code is not correct. Check it and try again.";
      case "already_set_up":
        return "SaveSpace is already set up. Sign in with your passkey.";
      case "setup_unavailable":
        return "Setup is not available on this server.";
      case "challenge_expired":
        return "The request took too long. Try again.";
      case "passkey_rejected":
      case "passkey_unknown":
        return "That passkey did not work here. Try another passkey or a recovery code.";
      case "recovery_code_invalid":
        return "That recovery code did not work. Each code works one time.";
      case "rate_limited":
        return "Too many tries. Wait one minute, then try again.";
      case "last_passkey":
        return "Add another passkey before you remove this one.";
      case "name_invalid":
        return "Use 1 to 100 characters.";
      case "aggregator_unavailable":
        return "The bank connection service did not answer. Try again in a few minutes.";
      case "aggregator_error":
        return "The bank connection service returned an error. Try again later.";
      case "splits_total":
        return "The parts must add up to the transaction amount.";
      case "splits_count":
        return "Use 2 to 10 parts.";
      case "split_amount_invalid":
        return "Each part must be more than zero.";
      case "transaction_pending":
        return "Split this transaction after it posts.";
      case "category_invalid":
        return "That category does not exist. Reload the page and try again.";
      case "note_invalid":
        return "Use up to 500 characters for the note.";
      case "merchant_name_invalid":
        return "Use up to 100 characters for the merchant name.";
      case "too_many_rules":
        return "You have the most rules SaveSpace allows. Delete one first.";
      case "rule_target_invalid":
        return "That merchant or category does not exist. Reload the page and try again.";
      default:
        return `Something went wrong. Try again. (Reference: ${error.requestId ?? error.code})`;
    }
  }
  return "Something went wrong. Check your connection and try again.";
}
