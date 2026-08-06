// Passkey (WebAuthn) glue for the login page and the profile page.
//
// The server speaks the WebAuthn JSON encoding (base64url strings); the
// browser API speaks ArrayBuffers. The two converters below bridge them
// by hand instead of relying on PublicKeyCredential.toJSON(), so the
// behavior is identical on every browser that has WebAuthn at all.
// Sections stay hidden unless support is confirmed — no dead buttons.
(function () {
    "use strict";

    if (!window.PublicKeyCredential || !navigator.credentials) return;

    function b64uToBuf(s) {
        s = s.replace(/-/g, "+").replace(/_/g, "/");
        var pad = s.length % 4 ? "=".repeat(4 - (s.length % 4)) : "";
        var bin = atob(s + pad);
        var bytes = new Uint8Array(bin.length);
        for (var i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
        return bytes.buffer;
    }

    function bufToB64u(buf) {
        var bytes = new Uint8Array(buf);
        var bin = "";
        for (var i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
        return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
    }

    async function postJson(url, body) {
        var res = await fetch(url, {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: body === undefined ? "{}" : JSON.stringify(body),
        });
        var data = null;
        try { data = await res.json(); } catch (e) { /* non-JSON error page */ }
        if (!res.ok) {
            throw new Error((data && data.error) || "Something went wrong. Try again.");
        }
        return data;
    }

    function showError(root, message) {
        var el = root.querySelector(".passkey-error");
        if (!el) return;
        el.textContent = message;
        el.hidden = false;
    }

    function clearError(root) {
        var el = root.querySelector(".passkey-error");
        if (el) el.hidden = true;
    }

    // The browser throws NotAllowedError both for "user hit cancel" and
    // for timeouts. Neither deserves a scary message.
    function friendly(err) {
        if (err && err.name === "NotAllowedError") {
            return "That was canceled. Tap the button to try again.";
        }
        if (err && err.name === "InvalidStateError") {
            return "This device already has a passkey for your account.";
        }
        return (err && err.message) || "Something went wrong. Try again.";
    }

    // ---- Sign in (login page) --------------------------------------------

    var loginRoot = document.querySelector("[data-passkey-login]");
    if (loginRoot) {
        loginRoot.hidden = false;
        var loginBtn = loginRoot.querySelector(".passkey-btn");
        loginBtn.addEventListener("click", async function () {
            clearError(loginRoot);
            loginBtn.disabled = true;
            try {
                var start = await postJson("/login/passkey/start");
                var pk = start.options.publicKey;
                var request = {
                    challenge: b64uToBuf(pk.challenge),
                    timeout: pk.timeout,
                    rpId: pk.rpId,
                    userVerification: pk.userVerification,
                };
                var cred = await navigator.credentials.get({ publicKey: request });
                var payload = {
                    ceremony: start.ceremony,
                    credential: {
                        id: cred.id,
                        rawId: bufToB64u(cred.rawId),
                        type: cred.type,
                        response: {
                            authenticatorData: bufToB64u(cred.response.authenticatorData),
                            clientDataJSON: bufToB64u(cred.response.clientDataJSON),
                            signature: bufToB64u(cred.response.signature),
                            userHandle: cred.response.userHandle
                                ? bufToB64u(cred.response.userHandle)
                                : null,
                        },
                        extensions: cred.getClientExtensionResults(),
                    },
                };
                var done = await postJson("/login/passkey/finish", payload);
                window.location.assign(done.redirect || "/app");
            } catch (err) {
                showError(loginRoot, friendly(err));
            } finally {
                loginBtn.disabled = false;
            }
        });
    }

    // ---- Register (profile page) -----------------------------------------

    var regRoot = document.querySelector("[data-passkey-register]");
    if (regRoot) {
        regRoot.hidden = false;
        var regBtn = regRoot.querySelector(".passkey-btn");
        regBtn.addEventListener("click", async function () {
            clearError(regRoot);
            regBtn.disabled = true;
            try {
                var start = await postJson("/app/profile/passkeys/register/start");
                var pk = start.options.publicKey;
                var creation = {
                    challenge: b64uToBuf(pk.challenge),
                    rp: pk.rp,
                    user: {
                        id: b64uToBuf(pk.user.id),
                        name: pk.user.name,
                        displayName: pk.user.displayName,
                    },
                    pubKeyCredParams: pk.pubKeyCredParams,
                    timeout: pk.timeout,
                    attestation: pk.attestation,
                    authenticatorSelection: pk.authenticatorSelection,
                    excludeCredentials: (pk.excludeCredentials || []).map(function (c) {
                        return { type: c.type, id: b64uToBuf(c.id), transports: c.transports };
                    }),
                };
                var cred = await navigator.credentials.create({ publicKey: creation });
                var labelInput = regRoot.querySelector("input[name=label]");
                var payload = {
                    ceremony: start.ceremony,
                    webauthnId: start.webauthnId,
                    label: labelInput ? labelInput.value : "",
                    credential: {
                        id: cred.id,
                        rawId: bufToB64u(cred.rawId),
                        type: cred.type,
                        response: {
                            attestationObject: bufToB64u(cred.response.attestationObject),
                            clientDataJSON: bufToB64u(cred.response.clientDataJSON),
                        },
                        extensions: cred.getClientExtensionResults(),
                    },
                };
                await postJson("/app/profile/passkeys/register/finish", payload);
                var doneEl = regRoot.querySelector(".passkey-done");
                if (doneEl) doneEl.hidden = false;
                // Show the new passkey in the list above.
                window.setTimeout(function () { window.location.reload(); }, 900);
            } catch (err) {
                showError(regRoot, friendly(err));
                regBtn.disabled = false;
            }
        });
    }
})();
