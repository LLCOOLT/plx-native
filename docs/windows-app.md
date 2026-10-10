# PlxNative for Windows

A native Plex client, built for LG webOS televisions, running here as a Windows app. Everything it
needs is in this folder: there is nothing to install.

## Open it (the first time only)

1. Unzip **PlxNative-…-windows-x64.zip** anywhere you like, and keep the `PlxNative` folder
   together: the program reads its fonts and libraries from the folder it sits in.
2. Double-click **PlxNative.exe**.
3. Windows SmartScreen will say it "protected your PC", because the app is not signed with a paid
   code-signing certificate. Click **More info**, then **Run anyway**. You do this once.
4. If Windows Firewall asks, allow the app on **private networks**: it talks to your Plex Media
   Server directly and cannot work without it.

## Sign in

The app shows a QR code and a four-character code. Scan the code with your phone, or go to
**plex.tv/link** and type it in. It then finds your server by itself and drops you on the home
screen. It stays signed in: you do this once.

You need to be on the **same network as your Plex server**. This is a television app: it talks to
the server directly over the LAN.

## Getting around

It is a TV interface, so it is driven like one, with the arrow keys.

| Key | Does |
|---|---|
| ↑ ↓ ← → | Move focus |
| Enter | Open / select |
| Esc | Back |
| Alt-F4 | Quit |

Clicking works too, on most screens.

## Watching

Press Play and it plays: the app decodes with its own bundled FFmpeg, on the PC's hardware video
decoder (Direct3D 11), and plays the sound through the current Windows output. H.264 and HEVC
(including 10-bit and HDR10, tone-mapped for an ordinary display), with AAC, AC-3, E-AC-3 and DTS
sound. Seeking, pause and resume work as they do on the television.

Text and Blu-ray/DVD image subtitles are drawn by the app. **Styled (ASS/SSA) subtitles are not
available in the Windows build yet**; the Mac and television builds have them.

## Small print

Windows 10 or 11, 64-bit (x64).

Unofficial, and not affiliated with, endorsed by, or sponsored by Plex GmbH or LG Electronics. MIT
licensed; the licence and the third-party notices are in this folder.
