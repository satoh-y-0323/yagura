# macOS Code Signing and Notarization

This guide explains how to sign, notarize, and staple a macOS build of yagura, where the signing
settings live in the Tauri configuration, and what a user has to do to open an unsigned build.

> **No yagura build is signed or notarized.** This page is a procedure, not a record. Nothing here
> has been carried out for this repository yet: `gui/src-tauri/tauri.conf.json` has no `bundle.macOS`
> section, its `bundle.targets` list contains no macOS target, and no Apple Developer account is
> associated with the project. Every command below is written to be run by whoever prepares the first
> signed macOS release.

Two facts decide most of what follows:

- **Signing and notarization can only be done on a Mac.** `codesign`, `notarytool`, and `stapler` are
  Apple tools that ship with the Xcode command line tools. Check that they are installed with
  `xcrun notarytool --version`; if that fails, install or update the command line tools
  (`xcode-select --install`).
- **Signing alone is not enough.** Since Apple introduced notarization for software distributed
  outside the App Store, a signed-but-not-notarized build is still blocked on a user's machine.
  Signing (section 3), notarization (section 4), and stapling (section 4) are one pipeline, and the
  hardened runtime (section 3) is a precondition for notarization to accept the submission.

This guide deliberately states no macOS release numbers. The minimum supported macOS version for
a build is the oldest version it has actually been verified on, on real hardware; see
"minimumSystemVersion" in section 5.

## 1. Getting a Developer ID Application Certificate

### Enrollment

A Developer ID certificate requires a paid **Apple Developer Program** membership. The membership
costs **99 USD per year** for both the individual and the organization enrollment (an equivalent
amount in local currency; Apple's enrollment page shows the exact figure and the taxes that apply,
and Apple can change it, so confirm the current price at enrollment time). The membership renews
annually — if it lapses, existing certificates stop being usable for new signatures.

- **Individual** enrollment needs an Apple Account with two-factor authentication enabled.
- **Organization** enrollment additionally needs a legal entity, a D-U-N-S number, and someone with
  authority to sign for the entity; it takes longer to approve than the individual route.

Enrollment is not instant. Plan for it to be the long pole of a first release.

### Which certificate

Apple issues several certificate types. For yagura only one matters:

| Certificate | Used for | Needed here |
| ------------------------------ | -------------------------------------------------- | ----------- |
| **Developer ID Application**   | Signing an `.app`, its embedded binaries, and a `.dmg` distributed outside the App Store | **Yes** |
| Developer ID Installer         | Signing a `.pkg` installer                          | Only if a `.pkg` is ever shipped |
| Apple Development / Distribution | Local development and App Store submission        | No |

In an organization account, creating Developer ID certificates is restricted to the Account Holder
role; a member with only the Developer role cannot create one.

### Creating and installing it

The simplest route is Xcode: **Xcode > Settings > Accounts**, select the team, **Manage
Certificates…**, then **+ > Developer ID Application**. Xcode generates the key pair and installs
both halves into the login keychain.

Without Xcode, create a Certificate Signing Request in **Keychain Access > Certificate Assistant >
Request a Certificate From a Certificate Authority**, upload it at
<https://developer.apple.com/account/resources/certificates>, download the issued `.cer`, and
double-click it to import. The private key stays on the machine that created the request — that
machine is the only one that can sign until the identity is exported.

Confirm the identity is usable for signing:

```bash
security find-identity -v -p codesigning
```

The output line for the certificate looks like
`1) ABCD1234... "Developer ID Application: Your Name (TEAMID1234)"`. The quoted string is the
**signing identity** used everywhere below, and the parenthesized value is the **Team ID**, which
notarization also needs.

To sign from another machine or from CI, export the identity **with its private key** from Keychain
Access as a `.p12` file protected by a password. Treat that file and its password as secrets: never
commit them, and store them as encrypted CI secrets (see "Automating in CI" in section 5).

## 2. What Has To Be Signed

A macOS `.app` is a directory, and code signing applies to every Mach-O file inside it, not just the
top-level bundle. In a yagura bundle that means at least:

- the GUI executable in `Contents/MacOS/`;
- the **`yagura-daemon` and `yagura-mcp` sidecars**, declared as `bundle.externalBin` in
  `gui/src-tauri/tauri.conf.json` and staged by `gui/scripts/prepare-sidecar.mjs` under the host
  triple name (for example `yagura-daemon-aarch64-apple-darwin`) and placed in the bundle as plain
  `yagura-daemon` and `yagura-mcp` beside the GUI executable, the names the GUI and the daemon look
  each other up by;
- any framework or dynamic library that ends up inside the bundle.

Data files such as the detection TOMLs in `bundle.resources` are not signed individually; they are
covered by the bundle seal, which is why editing anything inside a signed `.app` breaks its
signature.

**Sign inside-out**: every nested executable first, the bundle last. `codesign --deep` is not a
substitute — Apple documents it as unsuitable for distribution signing, because it applies one set of
options and one entitlements file to everything it touches.

## 3. Signing with codesign

### The command

```bash
IDENTITY="Developer ID Application: Your Name (TEAMID1234)"
APP="gui/src-tauri/target/release/bundle/macos/yagura.app"

# 1. nested executables first (the daemon and MCP sidecars, plus anything else that is Mach-O)
codesign --force --timestamp --options runtime \
  --sign "$IDENTITY" \
  "$APP/Contents/MacOS/yagura-daemon"
codesign --force --timestamp --options runtime \
  --sign "$IDENTITY" \
  "$APP/Contents/MacOS/yagura-mcp"

# 2. then the bundle itself
codesign --force --timestamp --options runtime \
  --entitlements gui/src-tauri/entitlements.plist \
  --sign "$IDENTITY" \
  "$APP"
```

The three flags that matter for distribution:

- `--options runtime` enables the **hardened runtime**. Notarization rejects submissions whose
  executables were signed without it.
- `--timestamp` embeds a secure timestamp from Apple's timestamp server. It requires network access
  and is also required by notarization; without it the signature stops validating when the
  certificate expires.
- `--entitlements` is optional and should stay that way unless a measured failure requires it (see
  below).

To find every Mach-O that needs step 1, walk the bundle:

```bash
find "$APP/Contents" -type f -perm -u+x -exec sh -c 'file "$1" | grep -q Mach-O && echo "$1"' _ {} \;
```

### Entitlements under the hardened runtime

The hardened runtime blocks several capabilities by default; an entitlement re-enables one of them.
Each entitlement weakens a protection, so the correct starting point is **no entitlements file at
all**: build, sign, run, and add an entitlement only when a specific, observed failure calls for it.
The entitlements file is a plist:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
  <dict>
    <key>com.apple.security.cs.allow-jit</key>
    <true/>
  </dict>
</plist>
```

Candidates worth knowing about, with the symptom each one addresses:

| Entitlement | Grants | Consider it when |
| ----------- | ------ | ---------------- |
| `com.apple.security.cs.allow-jit` | Writable-executable memory through the JIT-mapping API | The WebView's JavaScript engine crashes or refuses to run JIT-compiled code |
| `com.apple.security.cs.allow-unsigned-executable-memory` | Unsigned executable pages in general | A crash on executable memory persists with `allow-jit` alone; broader and weaker, so prefer `allow-jit` |
| `com.apple.security.cs.disable-library-validation` | Loading libraries signed by another team or unsigned | A plug-in or dynamic library from outside the team fails to load |
| `com.apple.security.cs.allow-dyld-environment-variables` | `DYLD_*` environment variables | A launcher must inject `DYLD_*`; normally not needed |
| `com.apple.security.cs.debugger` | Attaching a debugger to other processes | Never for a release build |

yagura's job is to launch arbitrary user commands under a PTY. That is why the **App Sandbox is not
used here** — Developer ID distribution does not require it, and this guide assumes an unsandboxed
bundle. A child process started by the app inherits the app's sandbox but **not** its entitlements,
so an agent CLI launched in a window is subject to its own signature and to the user's TCC
permissions, not to the entitlements listed above.

macOS privacy permissions (Accessibility, Full Disk Access, Files and Folders, and similar) are a
separate mechanism from entitlements. If the app or a command it launches touches a protected
location, macOS prompts the user, and the prompt text comes from `NS*UsageDescription` keys in
`Info.plist` (see `infoPlist` in section 5). A signed and notarized bundle does not skip those
prompts.

### Verifying the signature

```bash
codesign --verify --strict --verbose=2 "$APP"     # seal intact, inside-out
codesign -dvvv --entitlements - "$APP"            # identity, team, flags, entitlements
```

In the second command's output, confirm the `Authority=Developer ID Application: …` chain, the
`TeamIdentifier`, a `Timestamp=` line, and `flags=0x10000(runtime)` — that flag is the hardened
runtime. `spctl` assessment is deferred to after notarization (section 4), because an
un-notarized build is expected to fail it.

## 4. Notarizing with notarytool and Stapling

Notarization uploads the signed artifact to Apple, which scans it and returns a ticket. Stapling
attaches that ticket to the artifact so that the first launch works even without network access.

### Storing credentials once

`notarytool` accepts either an Apple Account with an **app-specific password**, or an **App Store
Connect API key**. Either way, store the credentials in the keychain once and refer to them by
profile name afterwards, so no secret appears in later command lines or logs.

App-specific password (create it at <https://account.apple.com> under App-Specific Passwords — the
regular Apple Account password does not work):

```bash
xcrun notarytool store-credentials "yagura-notary" \
  --apple-id "you@example.com" \
  --team-id "TEAMID1234" \
  --password "abcd-efgh-ijkl-mnop"
```

App Store Connect API key (preferred for CI: it is scoped and revocable without touching the Apple
Account):

```bash
xcrun notarytool store-credentials "yagura-notary" \
  --key "/secure/path/AuthKey_XXXXXXXXXX.p8" \
  --key-id "XXXXXXXXXX" \
  --issuer "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
```

If the Apple Account belongs to more than one team, the team must be disambiguated — with
`--team-id` here, and with `providerShortName` in the Tauri configuration (section 5).

### Submitting

A bare `.app` cannot be uploaded; submit a `.zip`, `.dmg`, or `.pkg` containing it. Use `ditto` to
zip a bundle — `ditto` preserves the symlinks and extended attributes inside an `.app` that plain
`zip` mangles:

```bash
ditto -c -k --keepParent "$APP" yagura.zip

xcrun notarytool submit yagura.zip \
  --keychain-profile "yagura-notary" \
  --wait
```

`--wait` blocks until Apple returns a verdict and prints the submission id and a status of `Accepted`
or `Invalid`. When it is `Invalid`, the reason per file is in the log:

```bash
xcrun notarytool log <submission-id> --keychain-profile "yagura-notary"
```

The common causes are all section 3 mistakes: an executable signed without `--options runtime`, a
signature without a secure timestamp, a nested binary that was never signed, or a signature made
with a non-Developer-ID certificate.

### Stapling

A ticket cannot be stapled to a `.zip`. Staple the `.app` (and separately the `.dmg`, if one is
shipped), then re-package for distribution:

```bash
xcrun stapler staple "$APP"
xcrun stapler validate "$APP"
ditto -c -k --keepParent "$APP" yagura.zip   # re-zip the stapled bundle for release
```

If a `.dmg` is the release artifact, notarize the `.dmg` itself and staple the `.dmg` after the
verdict — do it in that order, since building the image after stapling the `.app` would leave the
image itself un-notarized.

### Final check

Confirm the result the way a user's machine will evaluate it:

```bash
spctl -a -vvv -t exec "$APP"
```

The expected output names the source as `Notarized Developer ID` and accepts the bundle. Testing on
a machine that never built the app — ideally after downloading the artifact through a browser, so it
carries the quarantine attribute — is the only check that reflects a real first launch.

## 5. Tauri Configuration (`bundle.macOS`)

All bundle settings live in `gui/src-tauri/tauri.conf.json`. Its current state is Windows-only:
`bundle.targets` is `["nsis"]`, `bundle.windows` holds the NSIS settings, and there is **no**
`bundle.macOS` key. Two things must therefore change for a macOS release.

**Bundle targets.** `app` produces the `.app` and `dmg` produces the disk image. Rather than editing
the shared `targets` list — which is also used by the Windows build — pass the targets on the macOS
build command:

```bash
cd gui
npm run build:web
npx tauri build --bundles app,dmg
```

The build runs `beforeBuildCommand`, which includes `node scripts/prepare-sidecar.mjs`, so the
daemon and MCP sidecars are staged for the host triple before bundling. A universal build
(`--target universal-apple-darwin`) needs a sidecar staged for **each** architecture, since Tauri
resolves `externalBin` by target triple.

**Signing keys.** Add a `macOS` object under `bundle`:

```json
{
  "bundle": {
    "macOS": {
      "signingIdentity": "Developer ID Application: Your Name (TEAMID1234)",
      "hardenedRuntime": true,
      "entitlements": "entitlements.plist",
      "providerShortName": "YourProviderShortName",
      "minimumSystemVersion": "<verified minimum>"
    }
  }
}
```

The keys accepted by `bundle.macOS` (Tauri CLI 2.x, the version pinned in `gui/package.json`):

| Key | What to put there |
| --- | --- |
| `signingIdentity` | The quoted identity string from `security find-identity -v -p codesigning`. Omit it to leave the build unsigned. |
| `hardenedRuntime` | Defaults to `true`, which is what notarization requires. Leave it alone unless there is a measured reason. |
| `entitlements` | Path to the entitlements plist, relative to `gui/src-tauri/`. Omit the key entirely when there are no entitlements — the preferred starting state (section 3). |
| `providerShortName` | Only needed when the Apple Account belongs to several teams; it disambiguates which one signs. |
| `minimumSystemVersion` | The `LSMinimumSystemVersion` written into `Info.plist`. Set it from the oldest macOS version the build has actually been verified on. Tauri applies its own default when the key is absent. |
| `infoPlist` | Path to a plist merged into the generated `Info.plist`. This is where `NS*UsageDescription` strings go if a privacy prompt is ever needed. |
| `frameworks` | Frameworks or `.dylib`s to copy into the bundle. Anything added here becomes another thing to sign (section 2). |
| `files` | Extra files to place under `Contents/`. |
| `bundleName`, `bundleVersion` | Override the bundle's display name and `CFBundleVersion`. |
| `exceptionDomain` | An ATS exception for one domain. yagura talks to a local daemon over a Unix socket and needs no HTTP exception. |
| `dmg` | Disk image layout: `background`, `windowPosition`, `windowSize`, `appPosition`, `applicationFolderPosition`. |

The bundle identifier is **not** part of this object: it comes from the top-level `identifier` key,
`io.github.satoh-y-0323.yagura`. Notarization associates submissions with that identifier, and changing it
after a release makes the new build a different application to macOS.

Tauri signs during `tauri build` when an identity is configured. Notarization and stapling are then
run on the produced artifact exactly as in section 4; doing those steps by hand keeps the verdict and
the log visible, which is worth more than the convenience when a submission comes back `Invalid`.

### Automating in CI

On a macOS runner, keep the identity out of the repository:

- Store the exported `.p12` as a base64 CI secret together with its password, decode it at job start,
  create a temporary keychain, import the identity into it, and unlock it for the build step.
- Store the App Store Connect API key (`.p8`, key id, issuer id) as separate secrets and pass them to
  `xcrun notarytool submit` directly instead of using a stored keychain profile.
- Tauri can also take the signing identity, provider short name, and team from the environment
  (`APPLE_SIGNING_IDENTITY`, `APPLE_PROVIDER_SHORT_NAME`, `APPLE_DEVELOPMENT_TEAM`) so the values
  never enter `tauri.conf.json`. Confirm the exact variable set against the Tauri CLI version pinned
  in `gui/package.json` before depending on it.
- Signing and notarization need network access to Apple (timestamp server and the notary service),
  and notarization is slow enough that it should not sit on a pull-request path.

## 6. Running an Unsigned Build (Gatekeeper)

Until the pipeline above is in place, macOS artifacts are unsigned, and a user has to override
Gatekeeper deliberately. Say so in the release notes: the override is the user's own security
decision, and asking for it without explanation is indistinguishable from what malware asks for.

**Quarantine is what triggers the block.** Browsers and other quarantine-aware tools tag downloaded
files with the `com.apple.quarantine` extended attribute, and Gatekeeper evaluates a bundle on its
first launch because of that tag. A build compiled locally, or copied over with `scp`, `rsync`, or
`git`, carries no such tag and simply opens.

Two different messages, two different fixes:

**"yagura cannot be opened because the developer cannot be verified."** The bundle has a valid
structure but no recognized signature. Either way works:

- Right-click (or Control-click) the app in Finder, choose **Open**, then **Open** again in the
  dialog. The plain double-click does not offer this; the choice is remembered per app.
- Or double-click, let it be blocked, then open **System Settings > Privacy & Security**, scroll to
  the Security section, and use **Open Anyway** for the app named there.

**"yagura is damaged and can't be opened. You should move it to the Trash."** Despite the wording,
this usually means a quarantined bundle with no usable signature rather than a corrupt download.
Remove the quarantine attribute:

```bash
xattr -d com.apple.quarantine /Applications/yagura.app
# if that reports no such attribute, clear the whole bundle recursively:
xattr -c -r /Applications/yagura.app
```

**On Apple silicon, code must carry at least an ad-hoc signature to run at all.** Binaries produced
by the local toolchain get one automatically, but a bundle whose contents were modified after being
signed can be killed on launch. Re-signing ad-hoc restores that minimum:

```bash
codesign --force --deep --sign - /Applications/yagura.app
```

An ad-hoc signature satisfies only the kernel's requirement — it is anonymous, and Gatekeeper still
treats the app as unidentified.

**Do not tell users to turn Gatekeeper off.** `spctl --master-disable` (the "Anywhere" setting)
lowers the protection for every application on the machine and long outlives the install it was meant
to enable. The per-app override above is the right scope.
