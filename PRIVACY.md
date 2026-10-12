# Privacy policy

Effective 12 October 2026. This policy covers Freemonplay Host and Freemonplay Screen (the
"apps"), on macOS, Windows and Linux.

## In short

Freemonplay has no accounts, servers, analytics, telemetry, advertising or automatic
update checks. It talks only to computers on your own network that you connect it to. No
information is sent to the project, its maintainer or anyone else.

## What the apps exchange, and with whom

Only between a host and the screens you connect to it, directly over your local network:

- **The picture, input and clipboard:**
  - The host's virtual screens are streamed as video to the screens.
  - Mouse, keyboard and touch input travels between the computers.
  - When clipboard sharing is on, copied text and images travel between them too.
  - Items that password managers mark as private are never shared. Clipboard sharing can
    be turned off in the host's menu.
  - None of this is stored. It's shown, applied or pasted, then discarded.
- **Pairing:** a screen proves it knows the host's 6 digit pairing code, then receives a
  token so it can reconnect without the code.
- **Connection details:** each side's computer name and, for showing connection quality,
  a description of its network link. That description gives the type, Wi-Fi band, signal
  strength and link speed; it never includes the network's name. Frame rates and delays
  are exchanged for the same reason.

All of this is encrypted in transit (QUIC with TLS 1.3).

**Discovery:** so screens can find it, the host announces itself on the local network using
multicast DNS (Bonjour). The announcement carries its name, a random identifier and the
protocol version. Any device on the same network can see it, as with printers and other
shared devices. Hosts started with `--no-mdns` don't announce themselves.

## What the apps store, on your own computers only

- **Host:**
  - its identity (a random ID, a self-generated certificate and key);
  - the pairing code;
  - each paired screen's name and a hash of its token;
  - your settings.
- **Screen:**
  - the hosts it has paired with, with their names and tokens;
  - your settings.
- **Log files:** they record app activity, including computer names, network addresses and
  connection statistics, but never screen contents, keystrokes or clipboard contents.
  - macOS: `~/Library/Logs/freemonplay`.
  - Windows: `%LOCALAPPDATA%\freemonplay`.
  - Linux: `~/.local/state/freemonplay`.

  They stay on your computer unless you choose to share them, for example in a bug report.

You can delete any of this at any time: "Forget Paired Screens" in the host's menu, or by
uninstalling the apps and deleting their folders.

## When the apps go online

Only when you ask them to:

- **Opening download pages:** the setup window's "Get Freemonplay Screen" link and the
  Windows display driver link open web pages on GitHub in your browser.
  [GitHub's privacy statement](https://docs.github.com/site-policy/privacy-policies/github-general-privacy-statement)
  applies there, as it does when you download the apps from GitHub.

## Changes to your system (Windows host)

The Windows host installer asks for administrator rights once. With them it:

- installs the Virtual Display Driver, a separate open source project, to create virtual
  monitors;
- adds a Windows Firewall rule so screens can connect;
- optionally starts Freemonplay Host when you sign in.

Uninstalling removes all three. The driver itself works only on your computer and sends
nothing over the network.

## Children

The apps collect no personal information from anyone, children included.

## Changes and contact

Changes to this policy are published in this file, with a new effective date, and their
history is visible in the repository. Questions: open an issue at
<https://github.com/moridin1138/sw-freemonplay/issues>.
