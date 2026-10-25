# Verisure for HomewrightHQ

An external integration for [HomewrightHQ](https://github.com/magnusdunker/homewrighthq) for Verisure alarms: alarm panel, Yale Doorman locks on the installation, climate sensors, door and window sensors and smart plugs.

Verisure has no official API for third parties. The integration uses the API the My Verisure app uses, in the same way as the python-verisure library, which Home Assistant's Verisure integration is built on. Verisure can change it without notice.

## Installation

In HomewrightHQ: **Settings → Integrations → From GitHub**. Paste

```
https://github.com/magnusdunker/homewrighthq-verisure
```

and press **Install**. Then add **Verisure** under **Add integration**, and log in with the email and password you use in the My Verisure app.

If the account has two-step login, Verisure sends a code by SMS or email (choose which in the form). Enter the code in the next step. The integration then asks Verisure to remember it, so a restart does not require a new code. If the account has several installations, you choose one; add the integration again for the others.

Under **Options** on the integration the alarm code can be saved. Then the alarm and locks can be controlled from automations and cards without entering the code. Without a saved code, the code must be entered with each command.

## What you get

- an alarm panel with `disarmed`, `armed_home` and `armed_away`, and arm home, arm away and disarm. `changed_by` is whoever last changed the alarm.
- a lock for each Yale Doorman (or other smart lock) on the installation, with lock and unlock, and a door sensor where the lock reports the door
- temperature and humidity from smoke detectors and other sensors that measure them
- a binary sensor for each door and window sensor (open/closed)
- a switch for each smart plug

Each sensor becomes its own device with the name from the app (e.g. "Stue"), connected through the installation.

## Updates

The integration reads everything with one request every minute. Under **Options** the interval can be set from 30 to 600 seconds. After a command the alarm shows `arming` or `disarming` and the lock `locking` or `unlocking`, until it is read again after 15 seconds.

The session is saved on the entry and renewed automatically. If it cannot be renewed, the integration logs in again with the password. If Verisure then requires a new code, HomewrightHQ shows it under issues, and the integration must be authorized again.

## Not tested against the real Verisure

The integration is written from python-verisure and tested against a fake Verisure with responses shaped like the real ones. These things are not checked:

- The addresses `automation01/02.verisure.com` for login and `m-api01/02.verisure.com/graphql`, and the header `APPLICATION_ID: PS_PYTHON`.
- The two-step login: that `/auth/login` answers with `stepUpToken`, that `/auth/mfa?type=phone` or `type=email` sends the code, that `/auth/mfa/validate` takes `{"token": kode}`, and that `/auth/trust` gives a cookie so later logins do not require a code.
- That `GET /auth/token` renews the session, and the names of the cookies. The integration saves every cookie Verisure sets, whatever its name.
- Which errors mean an expired session: HTTP 401 or the GraphQL error codes `SYS_00001` and `SYS_00004`.
- The GraphQL queries `fetchAllInstallations`, `ArmState`, `Climate`, `DoorWindow`, `SmartLock` and `SmartPlug`, and the mutations `armStateArmAway`, `armStateArmHome`, `armStateDisarm`, `DoorLock`, `DoorUnlock` (with `LockDoorInput`) and `SmartPlugSetState`.
- That several queries can be sent in one request, and that the responses come back in the same order.
- The values `ARMED_HOME`, `ARMED_AWAY` and `DISARMED`, `LOCKED` and `UNLOCKED`, `OPEN` and `CLOSE`, and `ON` and `OFF`.
- What a wrong alarm code looks like in the response. Any GraphQL error on a command is shown as a rejected command.

Cameras, photos from motion sensors, the locks' settings (auto-lock, sound), the event log and several installations in one entry are not included.

## Development

Built in Rust on the `thehome-plugin` SDK from [homewright-example-integration-rust](https://github.com/magnusdunker/homewrighthq-example-integration-rust).

- `src/api.rs`: login, two-step login, cookies and Verisure's GraphQL queries
- `src/model.rs`: Verisure's data and how it becomes states
- `src/lib.rs`: the connection to HomewrightHQ
- `tests/verisure.rs`: tests against a fake Verisure with the responses in `tests/fixtures`, and a fake HomewrightHQ in `tests/host`

```
cargo test
cargo build --release
```

A new version is released with a tag, e.g. `git tag v0.1.0 && git push origin v0.1.0`. The workflow builds for Windows, Linux (x86_64 and arm64, static musl) and macOS and puts the binaries and `homewright.json` on the release.

## License

MIT
