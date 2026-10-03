import { describe, expect, it } from "vitest";
import { errorKey, localizeError } from "./error-text";

describe("localizeError", () => {
  it("translates known gateway errors", () => {
    expect(localizeError("pt-BR", "Invalid password")).toBe("Senha incorreta.");
    expect(localizeError("en-US", "Cannot remove the last account")).toBe("The last account cannot be removed.");
  });

  it("maps dynamic messages by prefix without leaking the detail", () => {
    const text = localizeError("pt-BR", "Account registration failed: sqlite busy");
    expect(text).toBe("Não foi possível cadastrar a conta.");
    expect(text).not.toContain("sqlite");
  });

  it("treats browser fetch failures as a network error", () => {
    expect(errorKey("Failed to fetch")).toBe("error.network");
  });

  it("falls back to the raw text for unknown messages and to a generic one when empty", () => {
    expect(localizeError("pt-BR", "something new")).toBe("something new");
    expect(localizeError("pt-BR", "")).toBe("A requisição falhou.");
  });
});
