# Making the Mac app open without the malware warning

macOS shows *"Apple could not verify DropCut is free of malware"* for any app
that is not **signed with a Developer ID certificate and notarized by Apple**.
It does not mean the app is malicious. The release workflow already does both
steps automatically once it has your Apple credentials. Until then it builds an
unsigned app.

## One-time setup (about an hour, plus Apple's approval time)

1. **Join the Apple Developer Program** (US$99 a year): <https://developer.apple.com/programs/>.
   Individual enrolment is fine. Approval can take a day or two.

2. **Create a "Developer ID Application" certificate.**
   Xcode > Settings > Accounts > your team > *Manage Certificates* > **+** >
   *Developer ID Application*. (Or create one at developer.apple.com under
   Certificates, Identifiers & Profiles.)

3. **Export it.** Open *Keychain Access*, find "Developer ID Application: Your
   Name (TEAMID)", right-click > *Export*, save as `cert.p12`, and choose a
   password. Then turn it into text:

   ```bash
   base64 -i cert.p12 | pbcopy
   ```

4. **Make an app-specific password** at <https://account.apple.com> > Sign-In and
   Security > App-Specific Passwords. This is not your Apple ID password.

5. **Find your Team ID**: developer.apple.com > Membership details (10 characters).

6. **Add six secrets** to the GitHub repo (Settings > Secrets and variables >
   Actions > New repository secret), or with the GitHub CLI:

   | Secret | Value |
   |---|---|
   | `APPLE_CERTIFICATE` | the base64 text from step 3 |
   | `APPLE_CERTIFICATE_PASSWORD` | the password you chose in step 3 |
   | `APPLE_SIGNING_IDENTITY` | exactly `Developer ID Application: Your Name (TEAMID)` |
   | `APPLE_ID` | your Apple ID email |
   | `APPLE_PASSWORD` | the app-specific password from step 4 |
   | `APPLE_TEAM_ID` | your Team ID |

   ```bash
   gh secret set APPLE_SIGNING_IDENTITY --body "Developer ID Application: Your Name (TEAMID)"
   ```

7. **Release as usual** (`git tag v0.3.0 && git push origin v0.3.0`). The Mac job
   will sign, submit to Apple's notarization service, wait for the result and
   staple the ticket. Expect a few extra minutes.

Keep `cert.p12` and the passwords out of the repository. Delete the local
`cert.p12` once the secret is saved.

## How you will know it worked

* The build log says *Developer ID signing and notarization enabled.* If a
  secret is missing it says so as a warning and builds unsigned instead.
* When signing is on, the final step runs `codesign`, `stapler validate` and
  `spctl`, and fails the release if the app is not accepted. Apple also rejects
  notarization if any bundled program (the ffmpeg sidecars) is not signed
  correctly; the log then lists exactly which file and why.
* On a Mac, the downloaded `.dmg` opens, DropCut can be dragged to Applications,
  and the first launch shows only the normal "downloaded from the internet,
  open?" prompt.

## Until then (for people installing an unsigned build)

* **macOS 15 (Sequoia) and later:** open DropCut once, dismiss the warning, then
  *System Settings > Privacy & Security*, scroll down to the DropCut message and
  click **Open Anyway**. Right-click > Open no longer works on these versions.
* **macOS 14 and earlier:** right-click DropCut > **Open** > Open.
* If it says the app is **damaged**, run once in Terminal:
  `xattr -cr /Applications/DropCut.app`

Windows has the same kind of warning (SmartScreen) for unsigned installers; it
needs a code-signing certificate from a certificate authority and is a separate
step.
