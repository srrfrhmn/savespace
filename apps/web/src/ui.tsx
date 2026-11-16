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
      default:
        return `Something went wrong. Try again. (Reference: ${error.requestId ?? error.code})`;
    }
  }
  return "Something went wrong. Check your connection and try again.";
}
