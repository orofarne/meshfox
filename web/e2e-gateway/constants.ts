// Shared by `playwright.gateway.config.ts` and the specs. Kept out of the
// config because importing a config file runs it (temp dirs, fixture copies).
export const OPEN_PORT = 4640;
export const TOKEN_PORT = 4641;
export const TOKEN = "e2e-gateway-token";
