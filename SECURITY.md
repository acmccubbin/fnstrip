# Security

## Reporting a vulnerability

Please report security problems privately, not in a public issue: on GitHub, open the
**Security** tab of this repository and choose **Report a vulnerability**. Include what you
found, how to reproduce it, and what it lets someone do.

This is a one-person project maintained in spare time. I'll acknowledge reports as soon as I
can and keep you posted on a fix. Only the latest release is supported.

## What counts

fnstrip runs as an ordinary user process (no root, no setuid, no `unsafe` code). Worth
reporting:

- anything that lets **another user or process** make fnstrip run commands, read or write
  files, or crash: through T1Bridge's Touch ID state file, the desktop provider, D-Bus
  (media players), audio or camera input, or anything else it reads;
- anything on the project page (`docs/`) that runs code or leaks data in a visitor's browser.

Not a vulnerability: **the config file runs commands by design.** Button `command` and `status`
entries run with `sh -c` as you, so anyone who can edit your `~/.config/fnstrip/config.toml`
can run commands as you. Treat the file like a shell script, and read any config you didn't
write before using it.

Problems in T1Bridge itself (the hardware service, Touch ID, the drivers) belong upstream:
[standardagents/t1bridge](https://github.com/standardagents/t1bridge).
