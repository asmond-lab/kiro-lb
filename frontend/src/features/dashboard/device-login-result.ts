import { translate, type Language } from "./preferences";

export type RegisteredDeviceLogin = { accountId: string; initialized: boolean; signedOut?: string[] };
export type RegistrationMessage = { tone: "ok" | "warn"; text: string };

export function registrationMessage(language: Language, result: RegisteredDeviceLogin): RegistrationMessage {
  const added = translate(language, result.initialized ? "accounts.login.added" : "accounts.login.addedNotInit", {
    id: result.accountId,
  });
  const signedOut = result.signedOut ?? [];
  if (signedOut.length === 0) return { tone: "ok", text: added };
  return {
    tone: "warn",
    text: `${added} ${translate(language, "accounts.login.signedOut", { ids: signedOut.join(", ") })}`,
  };
}
