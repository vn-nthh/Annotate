# Satoshi

The settings UI uses Satoshi by Indian Type Foundry, under the included
[ITF Free Font License](FFL.txt). These are unmodified official OpenType fonts.

Run `scripts/prepare-fonts.ps1` from the repository root. It downloads the official
[Fontshare Satoshi archive](https://api.fontshare.com/v2/fonts/download/satoshi)
and verifies the SHA-256 of its Light, Regular, Medium and Bold originals before
writing the ignored local files. The native build embeds these files in the app.
Raw font files are deliberately excluded from the repository and portable ZIP.

If Fontshare updates the originals, review its license and check the UI before
changing the hashes in the preparation script.
