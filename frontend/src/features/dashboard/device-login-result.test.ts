import { describe, expect, it } from "vitest";
import { registrationMessage } from "./device-login-result";

describe("registrationMessage", () => {
  it("confirms the new account when no other account was signed out", () => {
    const message = registrationMessage("en-US", { accountId: "a3e937d3fedb", initialized: true, signedOut: [] });

    expect(message.tone).toBe("ok");
    expect(message.text).toContain("a3e937d3fedb");
  });

  it("warns and names every account the login signed out", () => {
    const message = registrationMessage("en-US", {
      accountId: "a3e937d3fedb",
      initialized: true,
      signedOut: ["1ff88c428077", "be4371c9613f"],
    });

    expect(message.tone).toBe("warn");
    expect(message.text).toContain("a3e937d3fedb");
    expect(message.text).toContain("1ff88c428077, be4371c9613f");
  });

  it("treats a response without signedOut as a clean registration", () => {
    expect(registrationMessage("en-US", { accountId: "x", initialized: false }).tone).toBe("ok");
  });
});
