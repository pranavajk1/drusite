// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The sign-in and account pages. Plain DOM, no framework, no third-party code: these pages
// handle passwords and passkeys, so everything they run is in this file.
"use strict";

const app = document.getElementById("app");
const params = new URLSearchParams(location.search);
const request = params.get("request");

function h(tag, attrs, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (v === true) el.setAttribute(k, "");
    else if (v !== false && v != null) el.setAttribute(k, v);
  }
  for (const c of children.flat()) {
    if (c != null && c !== false) el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return el;
}

function show(...children) {
  app.replaceChildren(h("h1", {}, "Drusite"), ...children);
}

let errorLine = null;
function setError(message) {
  if (errorLine) errorLine.textContent = message || "";
}

async function api(path, body, method) {
  const response = await fetch(path, {
    method: method || (body === undefined ? "GET" : "POST"),
    headers: body === undefined ? {} : { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
    credentials: "same-origin",
  });
  const data = await response.json().catch(() => ({}));
  if (!response.ok) {
    const error = new Error(data.message || `The server answered ${response.status}`);
    error.code = data.error;
    error.status = response.status;
    throw error;
  }
  return data;
}

// Runs an action from a form or button, showing errors and disabling the button meanwhile.
function action(fn) {
  return async (event) => {
    event.preventDefault();
    const button = event.submitter || event.currentTarget;
    if (button && "disabled" in button) button.disabled = true;
    setError("");
    try {
      await fn(event);
    } catch (e) {
      if (e.name === "NotAllowedError") setError("The passkey prompt was closed.");
      else if (e.name === "InvalidStateError") setError("This device already has a passkey for your account.");
      else setError(e.message);
    } finally {
      if (button && "disabled" in button) button.disabled = false;
    }
  };
}

function field(name, label, type, extra) {
  return h("label", {}, label, h("input", { name, type, required: true, ...(extra || {}) }));
}

function value(form, name) {
  return form.elements[name].value;
}

// Every successful step answers {status, ...}: signed in, or a second step to show.
function step(result) {
  switch (result.status) {
    case "signed_in":
      location.assign(result.next);
      return;
    case "totp_required":
      return showTotp(result.challenge, "/api/v1/auth/password/totp");
    case "totp_setup_required":
      return showTotpSetup(result, "/api/v1/auth/password/totp-setup");
  }
}

// WebAuthn JSON: webauthn-rs sends base64url strings where the browser wants bytes.
const b64 = {
  decode(s) {
    const text = atob(s.replace(/-/g, "+").replace(/_/g, "/"));
    return Uint8Array.from(text, (c) => c.charCodeAt(0)).buffer;
  },
  encode(buffer) {
    const text = String.fromCharCode(...new Uint8Array(buffer));
    return btoa(text).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  },
};

function creationOptions(json) {
  const o = structuredClone(json.publicKey);
  o.challenge = b64.decode(o.challenge);
  o.user.id = b64.decode(o.user.id);
  o.excludeCredentials = (o.excludeCredentials || []).map((c) => ({ ...c, id: b64.decode(c.id) }));
  return { publicKey: o };
}

function requestOptions(json) {
  const o = structuredClone(json.publicKey);
  o.challenge = b64.decode(o.challenge);
  o.allowCredentials = (o.allowCredentials || []).map((c) => ({ ...c, id: b64.decode(c.id) }));
  const options = { publicKey: o };
  if (json.mediation) options.mediation = json.mediation;
  return options;
}

function credentialJson(credential) {
  const r = credential.response;
  const response = { clientDataJSON: b64.encode(r.clientDataJSON) };
  if (r.attestationObject) {
    response.attestationObject = b64.encode(r.attestationObject);
    if (r.getTransports) response.transports = r.getTransports();
  } else {
    response.authenticatorData = b64.encode(r.authenticatorData);
    response.signature = b64.encode(r.signature);
    response.userHandle = r.userHandle ? b64.encode(r.userHandle) : null;
  }
  return {
    id: credential.id,
    rawId: b64.encode(credential.rawId),
    type: credential.type,
    response,
    extensions: credential.getClientExtensionResults ? credential.getClientExtensionResults() : {},
  };
}

async function passkeyLogin() {
  const { challenge, options } = await api("/api/v1/auth/passkeys/login/start", {});
  const credential = await navigator.credentials.get(requestOptions(options));
  step(await api("/api/v1/auth/passkeys/login/finish", {
    challenge, credential: credentialJson(credential), request,
  }));
}

async function createPasskey(start, finish, extra) {
  const { challenge, options } = await api(start, extra || {});
  const credential = await navigator.credentials.create(creationOptions(options));
  return api(finish, { challenge, credential: credentialJson(credential), name: deviceName() });
}

function deviceName() {
  const ua = navigator.userAgent;
  if (/iPhone|iPad/.test(ua)) return "iPhone or iPad";
  if (/Android/.test(ua)) return "Android";
  if (/Mac OS X/.test(ua)) return "Mac";
  if (/Windows/.test(ua)) return "Windows";
  if (/Linux/.test(ua)) return "Linux";
  return "Passkey";
}

// The sign-in page: every method the server has on.
async function showSignIn(signup) {
  const [{ methods, selfSignup }, info] = await Promise.all([
    api("/api/v1/auth/methods"),
    request ? api(`/api/v1/auth/request/${encodeURIComponent(request)}`).catch(() => null) : null,
  ]);
  if (request && !info) {
    return show(h("p", { class: "error" },
      "This sign-in has expired or was already used. Go back to the app and start again."));
  }
  errorLine = h("p", { class: "error", role: "alert" });
  const parts = [];
  const has = (type) => methods.some((m) => m.type === type);

  if (has("passkeys") && window.PublicKeyCredential && !signup) {
    parts.push(h("button", { class: "primary", onclick: action(passkeyLogin) }, "Sign in with a passkey"));
  }
  for (const m of methods.filter((m) => m.type === "oidc")) {
    const href = `/api/v1/auth/oidc/${encodeURIComponent(m.id)}/start` +
      (request ? `?request=${encodeURIComponent(request)}` : "");
    parts.push(h("a", { class: "button", href }, `Continue with ${m.displayName}`));
  }
  if (has("password")) {
    if (parts.length) parts.push(h("div", { class: "divider" }, "or"));
    parts.push(signup ? passwordSignupForm() : passwordForm());
  }
  if (has("magicLink")) {
    if (parts.length) parts.push(h("div", { class: "divider" }, "or"));
    parts.push(magicLinkForm());
  }
  if (selfSignup && signup && has("passkeys") && window.PublicKeyCredential) {
    parts.push(h("div", { class: "divider" }, "or"), passkeySignupForm());
  }
  if (selfSignup && (has("password") || has("passkeys"))) {
    parts.push(h("p", { class: "muted" },
      signup ? "Have an account? " : "New here? ",
      h("a", { href: "#", onclick: (e) => { e.preventDefault(); showSignIn(!signup); } },
        signup ? "Sign in" : "Create an account")));
  }
  show(
    h("p", { class: "muted" }, info ? `Sign in to continue to ${info.client}.` : "Sign in to your account."),
    ...parts,
    errorLine,
  );
}

function passwordForm() {
  return h("form", { onsubmit: action(async (e) => {
      const f = e.target;
      step(await api("/api/v1/auth/password/login", {
        email: value(f, "email"), password: value(f, "password"), request,
      }));
    }) },
    field("email", "Email", "email", { autocomplete: "username webauthn" }),
    field("password", "Password", "password", { autocomplete: "current-password" }),
    h("button", { type: "submit" }, "Sign in"));
}

function passwordSignupForm() {
  return h("form", { onsubmit: action(async (e) => {
      const f = e.target;
      step(await api("/api/v1/auth/password/signup", {
        email: value(f, "email"), name: value(f, "name"), password: value(f, "password"), request,
      }));
    }) },
    field("name", "Name", "text", { autocomplete: "name", required: false }),
    field("email", "Email", "email", { autocomplete: "email" }),
    field("password", "Password (at least 10 characters)", "password",
      { autocomplete: "new-password", minlength: 10 }),
    h("button", { type: "submit" }, "Create account"));
}

function passkeySignupForm() {
  return h("form", { onsubmit: action(async (e) => {
      const f = e.target;
      step(await createPasskey("/api/v1/auth/passkeys/signup/start", "/api/v1/auth/passkeys/signup/finish",
        { email: value(f, "email"), name: value(f, "name"), request }));
    }) },
    field("name", "Name", "text", { autocomplete: "name", required: false }),
    field("email", "Email", "email", { autocomplete: "email" }),
    h("button", { type: "submit" }, "Create account with a passkey"));
}

function magicLinkForm() {
  return h("form", { onsubmit: action(async (e) => {
      const email = value(e.target, "email");
      await api("/api/v1/auth/magic-link/start", { email, request });
      show(h("p", {}, `If ${email} can sign in here, a link is on its way. Open it on this device.`));
    }) },
    field("email", "Email me a sign-in link", "email", { autocomplete: "email" }),
    h("button", { type: "submit" }, "Send link"));
}

function showTotp(challenge, path) {
  errorLine = h("p", { class: "error", role: "alert" });
  show(
    h("p", {}, "Enter the 6-digit code from your authenticator app."),
    h("form", { onsubmit: action(async (e) => {
        step(await api(path, { challenge, code: value(e.target, "code") }));
      }) },
      field("code", "Code", "text", { inputmode: "numeric", autocomplete: "one-time-code", pattern: "[0-9 ]{6,7}", autofocus: true }),
      h("button", { type: "submit", class: "primary" }, "Continue")),
    errorLine);
}

function showTotpSetup(setup, path) {
  errorLine = h("p", { class: "error", role: "alert" });
  show(
    h("p", {}, "This server requires an authenticator app. Add this key to yours, then enter the code it shows."),
    h("p", {}, h("a", { href: setup.otpauthUri }, "Open in authenticator app")),
    h("p", { class: "muted" }, "Or type the key: ", h("code", { class: "secret" }, setup.secret)),
    h("form", { onsubmit: action(async (e) => {
        step(await api(path, { challenge: setup.challenge, code: value(e.target, "code") }));
      }) },
      field("code", "Code", "text", { inputmode: "numeric", autocomplete: "one-time-code", autofocus: true }),
      h("button", { type: "submit", class: "primary" }, "Continue")),
    errorLine);
}

// A link from an email or from the server's administrator: show whose it is, then use it.
async function showLink(kind) {
  const token = new URLSearchParams(location.hash.slice(1)).get("token");
  history.replaceState(null, "", location.pathname);
  errorLine = h("p", { class: "error", role: "alert" });
  if (!token) return show(h("p", { class: "error" }, "This link is incomplete."));
  const base = kind === "magic" ? "/api/v1/auth/magic-link" : "/api/v1/auth/recover";
  let info;
  try {
    info = await api(kind === "magic" ? `${base}/peek` : `${base}/peek`, { token });
  } catch (e) {
    return show(h("p", { class: "error" }, e.message));
  }
  const finish = kind === "magic" ? `${base}/finish` : base;
  const nameField = info.newAccount ? field("name", "Your name", "text", { required: false, autocomplete: "name" }) : null;
  show(
    h("p", {}, info.newAccount ? `Create an account for ${info.email}?` : `Sign in as ${info.email}?`),
    h("form", { onsubmit: action(async (e) => {
        const body = { token };
        if (nameField) body.name = value(e.target, "name");
        step(await api(finish, body));
      }) },
      nameField,
      h("button", { type: "submit", class: "primary" }, info.newAccount ? "Create account" : "Sign in")),
    h("p", { class: "muted" }, "If you didn't ask for this link, close this page."),
    errorLine);
}

// The account page: login methods and signed-in devices.
async function showAccount() {
  let me;
  try {
    me = await api("/api/v1/account");
  } catch (e) {
    if (e.status === 401) return showSignIn(false);
    throw e;
  }
  const { methods } = await api("/api/v1/auth/methods");
  const has = (type) => methods.some((m) => m.type === type);
  errorLine = h("p", { class: "error", role: "alert" });
  const sections = [h("p", {}, me.displayName ? `${me.displayName} · ${me.email}` : me.email)];

  if (has("passkeys") && window.PublicKeyCredential) {
    const passkeys = await api("/api/v1/account/passkeys");
    sections.push(h("h2", {}, "Passkeys"), h("ul", { class: "list" },
      passkeys.map((p) => h("li", {}, h("span", {}, p.name),
        h("button", { onclick: action(async () => {
          await api(`/api/v1/account/passkeys/${p.id}`, undefined, "DELETE");
          showAccount();
        }) }, "Remove")))),
      h("button", { onclick: action(async () => {
        await createPasskey("/api/v1/account/passkeys/register/start", "/api/v1/account/passkeys/register/finish");
        showAccount();
      }) }, "Add a passkey"));
  }
  if (has("password")) {
    sections.push(h("h2", {}, me.hasPassword ? "Change password" : "Set a password"),
      h("form", { onsubmit: action(async (e) => {
          const f = e.target;
          await api("/api/v1/account/password", {
            current: me.hasPassword ? value(f, "current") : null, password: value(f, "password"),
          });
          f.reset();
          setError("Password saved.");
        }) },
        me.hasPassword ? field("current", "Current password", "password", { autocomplete: "current-password" }) : null,
        field("password", "New password", "password", { autocomplete: "new-password", minlength: 10 }),
        h("button", { type: "submit" }, "Save")));
    sections.push(h("h2", {}, "Authenticator app"), me.hasTotp
      ? h("p", { class: "muted" }, "On. Password sign-ins ask for a code.")
      : h("button", { onclick: action(async () => {
          const setup = await api("/api/v1/account/totp/start", {});
          showTotpSetup(setup, "/api/v1/account/totp/confirm");
        }) }, "Set up an authenticator app"));
  }
  const all = await api("/api/v1/account/devices");
  const devices = all.filter((d) => !d.agent);
  const agents = all.filter((d) => d.agent);
  const signOut = (d, label) => h("button", { onclick: action(async () => {
    await api(`/api/v1/account/devices/${d.id}`, undefined, "DELETE");
    showAccount();
  }) }, label);
  sections.push(h("h2", {}, "Signed-in devices"), devices.length
    ? h("ul", { class: "list" }, devices.map((d) => h("li", {},
        h("span", {}, d.name || "Device", h("br"), h("small", { class: "muted" }, `last used ${new Date(d.lastUsedAt).toLocaleDateString()}`)),
        signOut(d, "Sign out"))))
    : h("p", { class: "muted" }, "None."));
  sections.push(h("h2", {}, "Agents"),
    h("p", { class: "muted" }, "Agents (MCP clients such as Claude Code) can read your notes, and change them if you allow it. They can never delete pages, and you can undo what they change in the app."),
    agents.length
      ? h("ul", { class: "list" }, agents.map((d) => h("li", {},
          h("span", {}, d.name || "Agent", h("br"), h("small", { class: "muted" },
            `${accessText(d.access)} · last used ${new Date(d.lastUsedAt).toLocaleDateString()}`)),
          signOut(d, "Disconnect"))))
      : h("p", { class: "muted" }, "None connected."),
    agentTokenForm());
  sections.push(h("p", {}, h("button", { onclick: action(async () => {
    await api("/api/v1/account/logout", {});
    location.assign("/login");
  }) }, "Sign out of this browser")));
  show(...sections, errorLine);
}

function accessText(access) {
  if (!access) return "";
  if (!access.write) return "reads only";
  const parts = ["reads and writes"];
  if (access.journal) parts.push("journal too");
  if (access.publish) parts.push("publishes pages");
  return parts.join(", ");
}

// A token for an agent that can't sign in with a browser, such as one on a server.
function agentTokenForm() {
  const out = h("div", {});
  const form = h("form", { onsubmit: action(async (e) => {
      const f = e.target;
      const made = await api("/api/v1/account/agent-tokens", {
        name: value(f, "name"),
        write: f.elements.access.value === "write",
        journal: f.elements.journal.checked,
        publish: f.elements.publish.checked,
        days: Number(value(f, "days")),
      });
      f.reset();
      out.replaceChildren(
        h("p", {}, "Give the agent this address and token. The token is shown only now."),
        h("label", {}, "Address", h("input", { readonly: true, value: made.mcpUrl })),
        h("label", {}, "Token (an Authorization: Bearer header)", h("input", { readonly: true, value: made.token })),
        h("p", { class: "muted" }, `It works until ${new Date(made.expiresAt).toLocaleDateString()}, or until you disconnect it above.`),
        h("button", { onclick: () => showAccount() }, "Done"));
    }) },
    h("p", {}, "For an agent that can't sign in with a browser (one on a server), make a token:"),
    field("name", "The agent's name", "text", { required: true, maxlength: 100, placeholder: "Hermes" }),
    h("label", { class: "choice" }, h("input", { type: "radio", name: "access", value: "read", checked: true }), " Read only"),
    h("label", { class: "choice" }, h("input", { type: "radio", name: "access", value: "write" }), " Read, add and edit pages"),
    h("label", { class: "choice" }, h("input", { type: "checkbox", name: "journal" }), " Edit journal entries too"),
    h("label", { class: "choice" }, h("input", { type: "checkbox", name: "publish" }), " Publish pages to the web"),
    field("days", "Lasts this many days", "number", { required: true, min: 1, max: 365, value: 90 }),
    h("button", { type: "submit" }, "Make a token"));
  out.append(form);
  return out;
}

async function main() {
  try {
    if (location.pathname === "/auth/magic") await showLink("magic");
    else if (location.pathname === "/auth/recover") await showLink("recover");
    else if (location.pathname === "/account") await showAccount();
    else await showSignIn(false);
  } catch (e) {
    show(h("p", { class: "error" }, e.message));
  }
}

main();
