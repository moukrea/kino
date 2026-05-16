# Android sideload keystore

`kino-dev.keystore` is committed to this repository by design. Sideloaded
APK updates must be signed by the same key as the previous install or the
Android package manager refuses to upgrade. Committing the keystore means
anyone who clones the repo can produce builds that reinstall over a user's
existing kino, which is the behavior PRD §F-018 requires for sideload-first
distribution.

**This is not a security control.** Anyone with the repo can sign as kino.
For app-store distribution (out of v1 scope) we would generate a private
keystore and store it as GitHub secrets.

## Parameters (locked by PRD §F-001)

| Field | Value |
|---|---|
| File | `kino-dev.keystore` |
| Store type | PKCS12 |
| Key algorithm | RSA 2048 |
| Alias | `kino-dev` |
| Store password | `kinodev` |
| Key password | `kinodev` |
| Validity | 10000 days |
| Distinguished name | `CN=kino dev, O=kino, C=FR` |

## Regenerating

If the keystore must be regenerated (e.g. signature certificate expired,
which won't happen until ~2053 given the validity above), run from the
repo root:

```sh
keytool -genkeypair \
  -keystore android/keystore/kino-dev.keystore \
  -alias kino-dev \
  -keyalg RSA -keysize 2048 \
  -validity 10000 \
  -storepass kinodev -keypass kinodev \
  -dname "CN=kino dev, O=kino, C=FR"
```

Regenerating breaks update-over-previous-install for every existing
sideloaded copy of kino. Users will have to uninstall before reinstalling.
Treat this as a hard breaking change.

## CI integration

Both `.github/workflows/ci.yml` and `.github/workflows/release.yml` reference
this keystore directly. No GitHub secrets are required to build a signed
release in v1.
