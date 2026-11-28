# Sharing captures to your own server

Settings, Sharing. Upload a screenshot or a replay clip and get a link back, with no third party in between: the file goes to a server you control.

The kinds of target:

- S3-compatible: AWS S3, MinIO, Garage, Ceph, Cloudflare R2, Backblaze B2. Large clips upload in parts, and the link is presigned or built from your public link base.
- SFTP: any SSH server, with a password, a key file or the agent. The server's host key is pinned once you trust it.
- WebDAV: any WebDAV collection. Nextcloud and ownCloud also get a public share link through their OCS API.
- FTP and FTPS: explicit (AUTH TLS) and implicit TLS, anonymous login too.
- Custom HTTP: a ShareX-style endpoint. POST or PUT, multipart or raw body, your own headers, the link read from the response body, a JSON pointer or a regex.

## Setting up a target

Settings, Sharing, Add target. Pick the kind, fill in the fields, save, then Test connection: it writes a small probe file, reads it back and deletes it.

Passwords, keys and tokens are sealed with the OS keyring (GNOME Keyring, KWallet, DPAPI on Windows). On a machine without a keyring, saving a secret fails rather than storing it in the clear.

The public link base decides what the link looks like. It is a template: `{name}` is the uploaded file's remote name, `{ext}` its extension. Where the backend makes links itself (S3 presigning, Nextcloud shares) you can leave it empty.

Examples:

- nginx serving `/var/www/shots` at `https://shots.example.com/`, reached over SFTP: folder `/var/www/shots`, public link base `https://shots.example.com/{name}`.
- MinIO with a public-read bucket: endpoint `https://minio.example.com`, bucket `shots`, public link base `https://minio.example.com/shots/{name}`.
- Nextcloud: the WebDAV URL `https://cloud.example.com/remote.php/dav/files/you/shots`, an app password, Nextcloud share links on. No public link base needed.

The SFTP form has a host-key check: fetch the key, look at its fingerprint, trust it. From then on a different key fails the upload with a mismatch warning, the way SSH does.

## Naming

Random names (the default) look like `k5nq8v2mpzxa.png` and say nothing about the game. Keep file name keeps the capture's own name. A template can mix both, with `{game}`, `{date}`, `{time}`, `{name}`, `{stem}`, `{ext}` and `{rand}`.

A link is unlisted, not private. Anyone who has it can open the file.

## Sharing

Everything uploads to the default target: the one you mark default in Settings, or the only enabled one. With several enabled targets and none default, sharing sends you to Settings to pick one. `--share-to` names another target from the terminal.

- In the fullscreen viewer, the share button or the `s` key.
- In the gallery, the share button in a tile's corner when you hover it.
- In the overlay, X on a capture in the screenshots grid or its viewer, or the share icon with the mouse.
- From a terminal or a key daemon:

```
heronite-rs --share <file> [--share-to <target>]
heronite-rs --share-last
```

`--share` prints the link when Heronited is not running. Inside a running instance the link is copied to the clipboard and a notification shows it. `--share-last` needs the running instance: it takes the newest capture of a running game, or the newest capture at all when no game is running. The URL form is `heronite://share-last`, so a key daemon can use either. There is no URL form of `--share`, so a web page cannot make the app upload a file it names.

While an upload runs, the share button shows a spinner and a second click does nothing. When it finishes, the notification carries the link and the link is on the clipboard.

## Self-signed certificates

Targets on a LAN often use their own certificate. Set the CA file (the PEM of your CA, or the certificate itself) in the target's TLS options. Turning verification off is accepted but flagged: only on a network you trust.

## History

Settings, Sharing, History lists the uploads with their links, newest first, up to 200. Copy link puts the link back on the clipboard. Delete from server removes the remote file (best effort) and the record. A custom HTTP endpoint without a delete URL cannot delete, so only the record goes.
