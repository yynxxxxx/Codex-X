# Tauri updater signing setup

Codex-X uses the official Tauri v2 updater. Every updater artifact is signed during the release build, and the desktop app verifies that signature against the public key committed in `apps/desktop/src-tauri/tauri.conf.json` before installation.

## One-time repository-owner setup

1. Generate and retain a dedicated updater key pair on a trusted machine:

   ```bash
   pnpm --dir apps/desktop tauri signer generate \
     --write-keys "$HOME/.tauri/codex-x-updater.key"
   ```

   Use a strong password when prompted. Back up the private key and password outside the repository.

2. Commit the generated public-key value as `plugins.updater.pubkey` in `apps/desktop/src-tauri/tauri.conf.json`:

   ```bash
   cat "$HOME/.tauri/codex-x-updater.key.pub"
   ```

3. Add the matching private key and password to the upstream GitHub repository's Actions secrets:

   ```bash
   gh secret set TAURI_SIGNING_PRIVATE_KEY < "$HOME/.tauri/codex-x-updater.key"
   gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD --body 'KEY_PASSWORD'
   ```

4. Keep the same key for future releases. Replacing the committed public key means already-installed versions that trust the previous key will reject updates signed only by the new key.

## Release validation

The release workflow performs these checks before publishing:

- the tag matches the application version;
- both signing secrets are present;
- macOS generates `.app.tar.gz` updater archives for Apple Silicon and Intel;
- Windows generates the signed `.msi.zip` updater archive;
- Linux generates signed `.deb` and `.rpm` updater packages;
- every signature identifies the same key committed in `tauri.conf.json`;
- every dynamic manifest references the expected HTTPS release asset and embeds its matching signature.

Run the configuration-only check locally with:

```bash
python3 scripts/validate_updater_release.py --config-only
```
