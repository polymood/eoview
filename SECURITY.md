# Security policy

## Reporting a vulnerability

Please do not report security problems in a public issue. Use GitHub's private reporting instead: open
the **Security** tab of the repository, then **Report a vulnerability**. Only the maintainer sees the
report. You will get an answer as soon as possible.

## Supported versions

Only the latest release gets security fixes. eoview updates itself, so most users have the latest
release a few days after it comes out.

## Updates

Every release file is signed with an ed25519 key. eoview checks the signature before it installs an
update, and it refuses an update that is not signed with the key of the project.
