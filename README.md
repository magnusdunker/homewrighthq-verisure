# Verisure til HomewrightHQ

En ekstern integration til [HomewrightHQ](https://github.com/magnusdunker/homewrighthq) for Verisure-alarmer: alarmpanel, Yale Doorman-låse på installationen, klimasensorer, dør- og vinduessensorer og smarte stik.

Verisure har intet officielt API til tredjeparter. Integrationen bruger det API, My Verisure-appen bruger, på samme måde som biblioteket python-verisure, som Home Assistants Verisure-integration bygger på. Verisure kan ændre det uden varsel.

## Installation

I HomewrightHQ: **Indstillinger → Integrationer → Fra GitHub**. Indsæt

```
https://github.com/magnusdunker/homewrighthq-verisure
```

og tryk **Installér**. Tilføj derefter **Verisure** under **Tilføj integration**, og log ind med den e-mail og adgangskode, du bruger i My Verisure-appen.

Har kontoen totrinslogin, sender Verisure en kode med SMS eller e-mail (vælg hvilken i formularen). Skriv koden i næste trin. Derefter beder integrationen Verisure om at huske den, så en genstart ikke kræver en ny kode. Har kontoen flere installationer, vælger du en; tilføj integrationen igen for de andre.

Under **Indstillinger** på integrationen kan alarmkoden gemmes. Så kan alarm og låse styres fra automationer og kort uden at skrive koden. Uden gemt kode skal koden skrives ved hver kommando.

## Hvad du får

- et alarmpanel med `disarmed`, `armed_home` og `armed_away`, og tilkobl hjemme, tilkobl ude og frakobl. `changed_by` er den, der sidst ændrede alarmen.
- en lås for hver Yale Doorman (eller anden smart lås) på installationen, med lås og lås op, og en dørsensor, hvor låsen oplyser døren
- temperatur og luftfugtighed fra røgdetektorer og andre sensorer, der måler dem
- en binær sensor for hver dør- og vinduessensor (åben/lukket)
- en kontakt for hvert smart stik

Hver sensor bliver sin egen enhed med navnet fra appen (f.eks. "Stue"), forbundet gennem installationen.

## Opdatering

Integrationen læser alt med én forespørgsel hvert minut. Under **Indstillinger** kan tiden sættes fra 30 til 600 sekunder. Efter en kommando viser alarmen `arming` eller `disarming` og låsen `locking` eller `unlocking`, til den læses igen efter 15 sekunder.

Sessionen gemmes på opsætningen og fornyes af sig selv. Kan den ikke fornyes, logger integrationen ind igen med adgangskoden. Kræver Verisure så en ny kode, viser HomewrightHQ det under problemer, og integrationen skal godkendes igen.

## Ikke afprøvet mod den rigtige Verisure

Integrationen er skrevet ud fra python-verisure og afprøvet mod en falsk Verisure med svar formet som de rigtige. Disse ting er ikke kontrolleret:

- Adresserne `automation01/02.verisure.com` til login og `m-api01/02.verisure.com/graphql`, og headeren `APPLICATION_ID: PS_PYTHON`.
- Totrinsloginet: at `/auth/login` svarer med `stepUpToken`, at `/auth/mfa?type=phone` eller `type=email` sender koden, at `/auth/mfa/validate` tager `{"token": kode}`, og at `/auth/trust` giver en cookie, så senere login ikke kræver kode.
- At `GET /auth/token` fornyer sessionen, og navnene på cookies. Integrationen gemmer alle cookies, Verisure sætter, uanset navn.
- Hvilke fejl der betyder udløbet session: HTTP 401 eller GraphQL-fejlkoderne `SYS_00001` og `SYS_00004`.
- GraphQL-forespørgslerne `fetchAllInstallations`, `ArmState`, `Climate`, `DoorWindow`, `SmartLock` og `SmartPlug`, og mutationerne `armStateArmAway`, `armStateArmHome`, `armStateDisarm`, `DoorLock`, `DoorUnlock` (med `LockDoorInput`) og `SmartPlugSetState`.
- At flere forespørgsler kan sendes i én anmodning, og at svarene kommer i samme rækkefølge.
- Værdierne `ARMED_HOME`, `ARMED_AWAY` og `DISARMED`, `LOCKED` og `UNLOCKED`, `OPEN` og `CLOSE`, og `ON` og `OFF`.
- Hvordan en forkert alarmkode ser ud i svaret. Enhver GraphQL-fejl på en kommando vises som afvist kommando.

Kameraer, fotos fra bevægelsessensorer, låsenes indstillinger (auto-lås, lyd), hændelseslog og flere installationer i én opsætning er ikke med.

## Udvikling

Bygget i Rust på SDK'en `thehome-plugin` fra [homewright-example-integration-rust](https://github.com/magnusdunker/homewrighthq-example-integration-rust).

- `src/api.rs`: login, totrinslogin, cookies og Verisures GraphQL-forespørgsler
- `src/model.rs`: Verisures data og hvordan de bliver til tilstande
- `src/lib.rs`: forbindelsen til HomewrightHQ
- `tests/verisure.rs`: tests mod en falsk Verisure med svarene i `tests/fixtures`, og en falsk HomewrightHQ i `tests/host`

```
cargo test
cargo build --release
```

En ny version udgives med et tag, f.eks. `git tag v0.1.0 && git push origin v0.1.0`. Workflowet bygger til Windows, Linux (x86_64 og arm64, statisk musl) og macOS og lægger binærerne og `homewright.json` på releasen.

## Licens

MIT
