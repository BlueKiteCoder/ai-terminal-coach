# Releasing AI Terminal Coach

AI Terminal Coach publishes macOS 13+ archives for Apple Silicon and Intel. A public release is
allowed only when every executable is signed with a Developer ID Application certificate and both
architecture archives have been accepted by Apple's notarization service. Dry runs are intentionally
ad-hoc signed and must never be presented as public downloads.

## What the workflow guarantees

`.github/workflows/release.yml` has two modes:

- Every relevant pull request and manual dry run builds both native architectures, verifies every
  packaged path, code signature, minimum macOS version, manifest and checksum, and builds the
  ad-hoc-signed archive twice to prove byte-for-byte reproducibility.
- A `vX.Y.Z` tag, or an explicit manual publish run for an existing tag, fails closed unless signing
  and notarization credentials are present. It signs with hardened runtime and secure timestamps,
  notarizes both ZIP archives, creates GitHub artifact attestations, then creates a draft Release,
  uploads every asset, and publishes only after all uploads succeed.

Developer ID signing includes a secure timestamp, so official signed archives are not expected to be
byte-identical across separate builds. Their Git commit, manifest, checksums, GitHub attestations and
Apple notarization result provide the release evidence instead.

## One-time repository setup

Join the Apple Developer Program, create a **Developer ID Application** certificate, and export it
as a password-protected PKCS#12 (`.p12`) file. Create an App Store Connect API key that can submit
notarization requests. Add these GitHub Actions repository secrets:

| Secret | Value |
|---|---|
| `MACOS_CERTIFICATE_P12_BASE64` | Base64 of the exported `.p12` file |
| `MACOS_CERTIFICATE_PASSWORD` | Password used when exporting the `.p12` file |
| `MACOS_SIGNING_IDENTITY` | Exact identity, for example `Developer ID Application: Name (TEAMID)` |
| `APPLE_NOTARY_API_KEY_BASE64` | Base64 of the App Store Connect `AuthKey_….p8` file |
| `APPLE_NOTARY_KEY_ID` | App Store Connect API key ID |
| `APPLE_NOTARY_ISSUER_ID` | App Store Connect issuer UUID |

Never put these values in source, a workflow input, an issue, a Release note, or a local `.env` file.

## Release checklist

1. Update `[workspace.package].version` in `Cargo.toml`, refresh `Cargo.lock` if required, and move
   the relevant `CHANGELOG.md` entries from `Unreleased` into a dated version section.
2. Merge the release preparation pull request into `main`. A release tag is accepted only when its
   commit is already reachable from `origin/main`.
3. Run a dual-architecture dry run from the Actions page, or with GitHub CLI:

   ```zsh
   gh workflow run release.yml --ref main -f publish=false
   gh run watch
   ```

4. Create one annotated or signed tag on the verified `main` commit and push it:

   ```zsh
   git switch main
   git pull --ff-only
   git tag -s v0.1.0 -m "AI Terminal Coach v0.1.0"
   git push origin v0.1.0
   ```

   If Git tag signing is not configured, use `git tag -a`; Developer ID signing, notarization and
   GitHub attestations are still mandatory for the downloadable binaries.

5. Watch the Release workflow. If it fails before creating a Release, fix the cause on `main`, use a
   new version/tag when source changed, and run again. The workflow refuses to replace assets in an
   existing Release.

For recovery of an existing unpublished tag whose earlier workflow stopped before Release creation:

```zsh
gh workflow run release.yml -f tag=v0.1.0 -f publish=true
```

## Local package checks

The local command creates a clearly labelled ad-hoc-signed test artifact for the current native
architecture:

```zsh
AICOACH_REQUIRE_HOTKEY=1 scripts/package-release.sh 0.1.0
scripts/verify-release-tag.sh v0.1.0
```

It is safe for testing but is not a substitute for the public workflow. Setting
`AICOACH_REQUIRE_SIGNED=1` without `AICOACH_SIGNING_IDENTITY` deliberately fails before compiling.

## Verify a downloaded release

Download both the archive and its architecture-specific `.sha256` file, then run:

```zsh
shasum -a 256 -c aicoach-0.1.0-macos-arm64.sha256
gh attestation verify aicoach-0.1.0-macos-arm64.zip \
  --repo BlueKiteCoder/ai-terminal-coach
unzip -q aicoach-0.1.0-macos-arm64.zip
codesign --verify --strict --verbose=2 aicoach-0.1.0/bin/aicoach
spctl --assess --type execute --verbose=4 aicoach-0.1.0/bin/aicoach
```

Use the `x86_64` filenames on Intel Macs. The checksum file contains entries for the ZIP and
`tar.gz`, so download both archives when using `shasum -c`; the repository verifier can validate
either archive independently.

## Homebrew status

`homebrew/aicoach.rb` is an honest HEAD-only development formula while no stable public Release and
tap exist. Homebrew 6 requires formulae to live in a tap, so maintainers can validate it in a
temporary local tap:

```zsh
brew tap-new --no-git BlueKiteCoder/aicoach-dev
install -m 0644 homebrew/aicoach.rb \
  "$(brew --repository BlueKiteCoder/aicoach-dev)/Formula/aicoach.rb"
brew install --HEAD BlueKiteCoder/aicoach-dev/aicoach
brew test BlueKiteCoder/aicoach-dev/aicoach --HEAD
brew untap BlueKiteCoder/aicoach-dev
```

After the first signed/notarized release, publish a separate `BlueKiteCoder/homebrew-aicoach` tap
using the attested `aicoach.rb` asset generated by the Release workflow. It contains the immutable
tagged source URL and its measured SHA-256. Test install, `aicoach install`, upgrade, restart and
uninstall on both architectures before marking the tap roadmap item complete.
