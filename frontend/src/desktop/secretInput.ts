import { invoke, isDesktop } from "../api/transport";

/**
 * Host-side secret input (desktop only): the app's `remote-credential
 * add|rotate --secret-stdin`. The host opens the operating system's file
 * picker, reads the chosen file itself and sends the secret to its runtime;
 * this window only ever sees what happened (desktop/src-tauri/src/secret.rs).
 */
export const hostSecretInput = isDesktop;

export type SecretOutcome = {
  state: "stored" | "cancelled" | "invalid" | "refused";
  file_name: string | null;
  status: number | null;
  body: string | null;
  reason: string | null;
};

const CREDENTIALS = "/api/lightagent/v1/remote-credentials";

/**
 * Add a grant (`grant` null) or rotate one, with a secret the host reads
 * from a file the person picks. Resolves with the sentence to show for a
 * stored or cancelled outcome; throws with the reason for anything else.
 */
export async function sealFromChosenFile(
  grant: string | null,
  fields: Record<string, unknown>,
  stored: (answer: Record<string, unknown>) => string,
): Promise<string> {
  const target = grant === null ? CREDENTIALS : `${CREDENTIALS}/${grant}/rotate`;
  const outcome = await invoke<SecretOutcome>("remote_credential_secret", { target, fields: JSON.stringify(fields) });
  let answer: Record<string, unknown> = {};
  try {
    answer = outcome.body ? (JSON.parse(outcome.body) as Record<string, unknown>) : {};
  } catch {
    answer = {};
  }
  const file = outcome.file_name ?? "the chosen file";
  switch (outcome.state) {
    case "cancelled":
      return "No file was chosen, so nothing was stored.";
    case "invalid":
      throw new Error(`${file} cannot be used: ${outcome.reason ?? "unreadable"}. Nothing was stored; choose a file that holds only the secret.`);
    case "refused":
      throw new Error(`${typeof answer.error === "string" ? answer.error : `The runtime refused it (${outcome.status ?? "no status"})`}. Nothing was stored.`);
    default:
      return `${stored(answer)} The app read the secret from ${file} itself and sealed it; this window never held it. ` +
        "The file is left as it was: delete it yourself if you no longer need it.";
  }
}
