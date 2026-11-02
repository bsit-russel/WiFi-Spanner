# Security

## Reporting a vulnerability

Please report security problems privately, not in a public issue. Email
[bugsplat@byte-size.co.za](mailto:bugsplat@byte-size.co.za) with "Security" in the subject and
describe what you found, the version (the app's *Report a bug...* window shows it) and how to
reproduce it. Please do not include real network names, passwords or other personal information.

The mailbox is not monitored directly. Reports are read and handled in order of how serious they
are, and sending one does not guarantee a reply or a fix. Fixes are released as new versions;
check each release's notes.

## Supported versions

Only the latest release is supported. The app can update itself from the releases page.

## What the app does on the network

- On start (unless switched off in Settings) and when you choose *Check for updates*, it asks
  GitHub's API for the list of releases. This shares your IP address with GitHub.
- When you choose to install an update, it downloads the AppImage from GitHub over HTTPS and
  checks it against the checksum GitHub publishes before replacing the running file.
- Scanning reads the Wi-Fi networks around you with `iw` and NetworkManager. Nothing from a scan
  or a project is sent anywhere.
