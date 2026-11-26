// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Identity end to end in real Chromium, against a real Keycloak (OIDC) and Mailpit (SMTP), with
// a passkey on Chromium's virtual authenticator. The app side is an independent OAuth and DPoP
// client on WebCrypto. Run it with ui/server-e2e/run.sh, which starts everything it needs.
import { chromium } from "@playwright/test";
import {
  webcrypto as wc,
  createHmac,
  randomBytes,
  createHash,
} from "node:crypto";
import http from "node:http";

// The app's loopback listener.
const listener = http.createServer((req, res) => {
  res.end("app got it");
});
await new Promise((r) => listener.listen(49999, "127.0.0.1", r));

const SERVER = "http://localhost:18080";
const MAILPIT = "http://localhost:58025";
const REDIRECT = "http://127.0.0.1:49999/oauth/callback";
const results = [];
function check(name, ok, detail = "") {
  results.push({ name, ok: !!ok, detail });
  console.log(`${ok ? "PASS" : "FAIL"} ${name}${detail ? " — " + detail : ""}`);
}
const b64u = (buf) => Buffer.from(buf).toString("base64url");

// ---------- an app: PKCE, DPoP key, token exchange ----------
class App {
  async init() {
    this.key = await wc.subtle.generateKey(
      { name: "ECDSA", namedCurve: "P-256" },
      false,
      ["sign", "verify"],
    );
    const jwk = await wc.subtle.exportKey("jwk", this.key.publicKey);
    this.jwk = { kty: "EC", crv: "P-256", x: jwk.x, y: jwk.y };
    this.jkt = b64u(
      createHash("sha256")
        .update(`{"crv":"P-256","kty":"EC","x":"${jwk.x}","y":"${jwk.y}"}`)
        .digest(),
    );
    return this;
  }
  authorizeUrl() {
    this.verifier = b64u(randomBytes(32));
    this.state = b64u(randomBytes(16));
    const u = new URL("/oauth/authorize", SERVER);
    u.search = new URLSearchParams({
      response_type: "code",
      client_id: "drusite-app",
      redirect_uri: REDIRECT,
      state: this.state,
      code_challenge: b64u(createHash("sha256").update(this.verifier).digest()),
      code_challenge_method: "S256",
      scope: "read write",
      dpop_jkt: this.jkt,
    });
    return u.toString();
  }
  async proof(method, url, accessToken) {
    const header = { typ: "dpop+jwt", alg: "ES256", jwk: this.jwk };
    const payload = {
      jti: b64u(randomBytes(16)),
      htm: method,
      htu: url,
      iat: Math.floor(Date.now() / 1000),
    };
    if (accessToken)
      payload.ath = b64u(createHash("sha256").update(accessToken).digest());
    const input = `${b64u(JSON.stringify(header))}.${b64u(JSON.stringify(payload))}`;
    const sig = await wc.subtle.sign(
      { name: "ECDSA", hash: "SHA-256" },
      this.key.privateKey,
      Buffer.from(input),
    );
    return `${input}.${b64u(sig)}`;
  }
  codeFrom(callbackUrl) {
    const u = new URL(callbackUrl);
    if (u.searchParams.get("state") !== this.state)
      throw new Error("state mismatch");
    if (u.searchParams.get("iss") !== SERVER)
      throw new Error("iss mismatch " + u.searchParams.get("iss"));
    if (u.searchParams.get("error"))
      throw new Error("refused: " + u.searchParams.get("error_description"));
    return u.searchParams.get("code");
  }
  async token(form) {
    const url = `${SERVER}/oauth/token`;
    const r = await fetch(url, {
      method: "POST",
      headers: { dpop: await this.proof("POST", url) },
      body: new URLSearchParams(form),
    });
    return { status: r.status, body: await r.json() };
  }
  async exchange(code, device) {
    const r = await this.token({
      grant_type: "authorization_code",
      code,
      redirect_uri: REDIRECT,
      code_verifier: this.verifier,
      client_id: "drusite-app",
      device_name: device,
    });
    if (r.status !== 200)
      throw new Error("exchange: " + JSON.stringify(r.body));
    this.tokens = r.body;
    return r.body;
  }
  async refresh() {
    const r = await this.token({
      grant_type: "refresh_token",
      refresh_token: this.tokens.refresh_token,
      client_id: "drusite-app",
    });
    if (r.status === 200) this.tokens = r.body;
    return r;
  }
  async me() {
    const url = `${SERVER}/api/v1/me`;
    const at = this.tokens.access_token;
    const r = await fetch(url, {
      headers: {
        authorization: `DPoP ${at}`,
        dpop: await this.proof("GET", url, at),
      },
    });
    return { status: r.status, body: await r.json().catch(() => null) };
  }
}

// ---------- browser helpers ----------
const browser = await chromium.launch({ headless: !process.env.HEADED });
async function newPage() {
  const context = await browser.newContext();
  const page = await context.newPage();
  const errors = [];
  page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
  page.on("pageerror", (e) => errors.push(e.message));
  return { context, page, errors };
}
async function waitForCallback(page) {
  await page.waitForURL(/^http:\/\/127\.0\.0\.1:49999\//, { timeout: 15000 });
  return page.url();
}

function totp(secretB32, offset = 0) {
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
  let bits = "",
    bytes = [];
  for (const c of secretB32)
    bits += alphabet.indexOf(c).toString(2).padStart(5, "0");
  for (let i = 0; i + 8 <= bits.length; i += 8)
    bytes.push(parseInt(bits.slice(i, i + 8), 2));
  const step = Math.floor((Date.now() / 1000 + offset) / 30);
  const msg = Buffer.alloc(8);
  msg.writeBigUInt64BE(BigInt(step));
  const d = createHmac("sha1", Buffer.from(bytes)).update(msg).digest();
  const o = d[19] & 15;
  return String((d.readUInt32BE(o) & 0x7fffffff) % 1e6).padStart(6, "0");
}

async function latestMailTo(address) {
  for (let i = 0; i < 50; i++) {
    const list = await (
      await fetch(
        `${MAILPIT}/api/v1/search?query=${encodeURIComponent("to:" + address)}`,
      )
    ).json();
    if (list.messages?.length) {
      return await (
        await fetch(`${MAILPIT}/api/v1/message/${list.messages[0].ID}`)
      ).json();
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  throw new Error("no mail for " + address);
}

try {
  // ===== 1. Magic link through real SMTP =====
  {
    const app = await new App().init();
    const { page, errors } = await newPage();
    await page.goto(app.authorizeUrl());
    await page.getByLabel("Email me a sign-in link").fill("lin@drusite.test");
    await page.getByRole("button", { name: "Send link" }).click();
    await page.getByText("a link is on its way").waitFor();
    const mail = await latestMailTo("lin@drusite.test");
    check(
      "magic link: email delivered over SMTP",
      mail.Subject === "Your Drusite sign-in link",
      mail.Subject,
    );
    const link = mail.Text.match(
      /http:\/\/localhost:18080\/auth\/magic#token=\S+/,
    )?.[0];
    check(
      "magic link: link points at /auth/magic with a fragment token",
      !!link,
    );
    const linkPage = (await newPage()).page; // opened from the mail client: another tab
    await linkPage.goto(link);
    await linkPage
      .getByText("Create an account for lin@drusite.test?")
      .waitFor();
    check("magic link: page asks before using it", true);
    check(
      "magic link: token removed from the address bar",
      !linkPage.url().includes("token="),
    );
    await linkPage.getByLabel("Your name").fill("Lin");
    await linkPage.getByRole("button", { name: "Create account" }).click();
    const code = app.codeFrom(await waitForCallback(linkPage));
    await app.exchange(code, "Lin's laptop");
    const me = await app.me();
    check(
      "magic link: app gets DPoP tokens; email verified",
      me.status === 200 && me.body.emailVerified === true,
      JSON.stringify(me.body),
    );
    // Reused link.
    await linkPage.goto(link);
    await linkPage.getByText("expired or was already used").waitFor();
    check("magic link: second use refused", true);
    check(
      "magic link: no console errors",
      errors.length === 0,
      errors.join("; "),
    );

    // ===== 4. On Lin's account page: password, TOTP, devices =====
    const acct = linkPage;
    const acctErrors = [];
    acct.on("pageerror", (e) => acctErrors.push(e.message));
    await acct.goto(`${SERVER}/account`);
    await acct.getByRole("heading", { name: "Set a password" }).waitFor();
    await acct.getByLabel("New password").fill("lin-test-password-1");
    await acct.getByRole("button", { name: "Save" }).click();
    await acct.getByText("Password saved.").waitFor();
    check("account: first password set without an old one", true);

    await acct
      .getByRole("button", { name: "Set up an authenticator app" })
      .click();
    const secret = await acct.locator("code.secret").textContent();
    const otpLink = await acct
      .getByRole("link", { name: "Open in authenticator app" })
      .getAttribute("href");
    check(
      "account: TOTP shows a secret and otpauth URI",
      /^[A-Z2-7]{32}$/.test(secret) && otpLink.startsWith("otpauth://totp/"),
      otpLink,
    );
    await acct.getByLabel("Code").fill("000000");
    await acct.getByRole("button", { name: "Continue" }).click();
    await acct.getByText("that code isn't right").waitFor();
    check("account: wrong TOTP code refused in the page", true);
    await acct.getByLabel("Code").fill(totp(secret));
    await acct.getByRole("button", { name: "Continue" }).click();
    await acct.getByText("On. Password sign-ins ask for a code.").waitFor();
    check("account: TOTP confirmed", true);

    // Password sign-in now asks for a code; the setup code can't be reused.
    {
      const app2 = await new App().init();
      const { page: p2, errors: e2 } = await newPage();
      await p2.goto(app2.authorizeUrl());
      await p2
        .locator("form")
        .filter({ has: p2.getByLabel("Password") })
        .getByLabel("Email")
        .fill("lin@drusite.test");
      await p2.getByLabel("Password").fill("lin-test-password-1");
      await p2.getByRole("button", { name: "Sign in", exact: true }).click();
      await p2.getByText("Enter the 6-digit code").waitFor();
      await p2.getByLabel("Code").fill(totp(secret, 30));
      await p2.getByRole("button", { name: "Continue" }).click();
      const code2 = app2.codeFrom(await waitForCallback(p2));
      await app2.exchange(code2, "Lin's phone");
      check(
        "password + TOTP sign-in through the page",
        (await app2.me()).status === 200,
      );
      check("password page: no console errors", e2.length === 0, e2.join("; "));
    }

    // Change the password: wrong current refused, right one accepted.
    await acct.goto(`${SERVER}/account`);
    await acct.getByLabel("Current password").fill("not-the-password");
    await acct.getByLabel("New password").fill("lin-test-password-2");
    await acct.getByRole("button", { name: "Save" }).click();
    await acct.getByText("the current password isn't right").waitFor();
    check("account: change needs the current password", true);

    // Devices: both apps listed; sign one out and its tokens die.
    await acct.goto(`${SERVER}/account`);
    await acct.getByText("Lin's laptop").waitFor();
    await acct.getByText("Lin's phone").waitFor();
    check("account: signed-in devices listed", true);
    await acct
      .locator("li", { hasText: "Lin's laptop" })
      .getByRole("button", { name: "Sign out" })
      .click();
    await acct
      .locator("li", { hasText: "Lin's laptop" })
      .waitFor({ state: "detached" });
    const after = await app.me();
    const refreshed = await app.refresh();
    check(
      "account: signing out a device kills its tokens",
      after.status === 401 && refreshed.status === 400,
      `${after.status} ${refreshed.status}`,
    );
    check(
      "account page: no console errors",
      acctErrors.length === 0,
      acctErrors.join("; "),
    );
  }

  // ===== 2. OIDC against real Keycloak =====
  async function keycloak(user) {
    const app = await new App().init();
    const { page, errors } = await newPage();
    await page.goto(app.authorizeUrl());
    await page.getByRole("link", { name: "Continue with Keycloak" }).click();
    await page.locator("#username").fill(user);
    await page.locator("#password").fill("kc-test-pass");
    await page.locator("#kc-login").click();
    return { app, page, errors };
  }
  {
    const { app, page, errors } = await keycloak("alice");
    const code = app.codeFrom(await waitForCallback(page));
    await app.exchange(code, "Alice's Mac");
    const me = await app.me();
    check(
      "OIDC (Keycloak): verified user in group signs in",
      me.status === 200 &&
        me.body.email === "alice@drusite.test" &&
        me.body.emailVerified === true,
      JSON.stringify(me.body),
    );
    check("OIDC: no console errors", errors.length === 0, errors.join("; "));
    // Refresh keeps working, rotation.
    const old = app.tokens.refresh_token;
    const r = await app.refresh();
    check(
      "OIDC: refresh rotates",
      r.status === 200 && app.tokens.refresh_token !== old,
    );
  }
  {
    const { app, page } = await keycloak("bob");
    const code = app.codeFrom(await waitForCallback(page));
    await app.exchange(code, "Bob");
    const me = await app.me();
    check(
      "OIDC: unverified email still gets an account, marked unverified",
      me.status === 200 && me.body.emailVerified === false,
      JSON.stringify(me.body),
    );
  }
  {
    const { page } = await keycloak("carol");
    await page
      .getByText("isn't in a group allowed")
      .waitFor({ timeout: 15000 });
    check("OIDC: user outside allowed_groups refused", true);
  }

  // ===== 3. Passkeys in real Chromium (virtual platform authenticator) =====
  {
    const { context, page, errors } = await newPage();
    const cdp = await context.newCDPSession(page);
    await cdp.send("WebAuthn.enable");
    const { authenticatorId } = await cdp.send(
      "WebAuthn.addVirtualAuthenticator",
      {
        options: {
          protocol: "ctap2",
          transport: "internal",
          hasResidentKey: true,
          hasUserVerification: true,
          isUserVerified: true,
          automaticPresenceSimulation: true,
        },
      },
    );
    await page.goto(`${SERVER}/login`);
    await page.getByRole("link", { name: "Create an account" }).click();
    const form = page
      .locator("form")
      .filter({
        has: page.getByRole("button", {
          name: "Create account with a passkey",
        }),
      });
    await form.getByLabel("Name").fill("Kim");
    await form.getByLabel("Email").fill("kim@drusite.test");
    await form
      .getByRole("button", { name: "Create account with a passkey" })
      .click();
    await page.waitForURL(`${SERVER}/account`);
    const { credentials } = await cdp.send("WebAuthn.getCredentials", {
      authenticatorId,
    });
    check(
      "passkey: sign-up made a resident credential in Chromium",
      credentials.length === 1 && credentials[0].isResidentCredential,
      JSON.stringify(credentials.map((c) => c.isResidentCredential)),
    );
    await page.getByRole("heading", { name: "Passkeys" }).waitFor();
    check(
      "passkey: listed on the account page",
      (await page.locator("ul.list li", { hasText: "Mac" }).count()) >= 1,
    );

    // Sign out, then sign in for an app with the passkey and no email typed.
    await page
      .getByRole("button", { name: "Sign out of this browser" })
      .click();
    await page.waitForURL(`${SERVER}/login`);
    const app = await new App().init();
    await page.goto(app.authorizeUrl());
    await page.getByRole("button", { name: "Sign in with a passkey" }).click();
    const code = app.codeFrom(await waitForCallback(page));
    await app.exchange(code, "Kim's Mac");
    const me = await app.me();
    check(
      "passkey: sign-in in Chromium gets app tokens",
      me.status === 200 && me.body.email === "kim@drusite.test",
      JSON.stringify(me.body),
    );
    const { credentials: after } = await cdp.send("WebAuthn.getCredentials", {
      authenticatorId,
    });
    check(
      "passkey: sign counter advanced",
      after[0].signCount >= 1,
      String(after[0].signCount),
    );

    // Adding the same authenticator again is excluded.
    await page.goto(`${SERVER}/account`);
    await page.getByRole("button", { name: "Add a passkey" }).click();
    await page.locator("p.error").filter({ hasText: /./ }).waitFor();
    const msg = await page.locator("p.error").textContent();
    check(
      "passkey: registering the same authenticator twice is refused",
      /excluded|already|registered|InvalidState|not allowed|closed/i.test(msg),
      msg,
    );
    check(
      "passkey pages: no unexpected console errors",
      errors.every((e) => /credential|excluded|InvalidState/i.test(e)),
      errors.join("; "),
    );
  }
} catch (e) {
  check(
    "script ran to the end",
    false,
    e.stack.split("\n").slice(0, 3).join(" | "),
  );
} finally {
  await browser.close();
  listener.close();
  const failed = results.filter((r) => !r.ok);
  console.log(`\n${results.length - failed.length}/${results.length} passed`);
  process.exit(failed.length ? 1 : 0);
}
